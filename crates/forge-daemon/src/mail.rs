//! The mail engine of the daemon (docs/04 §1.9, docs/09 §mail, ADR-0040, M2-E3-T7): the
//! captured e-mail of the instances whose manifest declares `spec.mail`. There is no code
//! per tool: the block names the API family (`mailpit-v1`), the port names and the `.env`
//! variables, and the family decides the adapter ([`Backend`]).
//!
//! * **Adapter** ([`MailpitV1`]): `reqwest` (rustls, no proxy, 2 s per request) against
//!   `http://127.0.0.1:<http_port>/api/v1`. Message and part ids are checked before they
//!   go into a URL. `mail.show` goes through the backend's "open a message" call, which
//!   marks it read; when the caller must not change the mailbox (`mark_read: false`, or a
//!   `viewer`) the previous unread state is read first (up to [`UNREAD_SCAN_MAX`] unread
//!   messages) and restored.
//! * **Attachments** over [`MAIL_ATTACHMENT_MAX_BYTES`] are never buffered: the download
//!   stops at the cap and the answer is `invalid_input` pointing at the backend's UI.
//! * **Watcher** ([`watch_loop`]): polls `/info` every 3 s ±10 % only while an instance
//!   runs and publishes `mail.changed {messages, unread, new_messages}` through the
//!   [`MailHub`], only when the counts move. Calls that change the mailbox feed the hub
//!   too, so clients never wait for the next tick. The first count of a run is a silent
//!   baseline.
//! * Every wait has a timeout; nothing here holds a lock across an `.await`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::InstanceId;
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_ipc::api::{
    MAIL_ATTACHMENT_MAX_BYTES, MailAttachmentContentDto, MailAttachmentDto, MailChangedPayload,
    MailListDto, MailListParams, MailMessageDto, MailStatusDto, MailSummaryDto,
};
use forge_manifest::schema::tool::{MailApi, MailSpec};
use forge_supervisor::{InstanceState, QUICK_ASK_TIMEOUT};
use reqwest::Method;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::daemon::Daemon;
use crate::scheduler::jittered;
use crate::services::Entry;

/// Time budget of one request to the backend.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Interval of the watcher (±10 %).
pub const WATCH_PERIOD: Duration = Duration::from_secs(3);
/// Default and largest page of `mail.list`.
pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 500;
/// Unread messages looked at to restore the unread state after `mail.show`.
pub const UNREAD_SCAN_MAX: u32 = 1000;
/// Longest id accepted in a URL.
const MAX_ID_LEN: usize = 128;

/// Counts of a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub messages: u32,
    pub unread: u32,
}

// ------------------------------------------------------------------ the adapter

/// The adapter chosen by `spec.mail.api`.
#[derive(Debug, Clone)]
pub enum Backend {
    MailpitV1(MailpitV1),
}

impl Backend {
    /// The adapter of `api` talking to `127.0.0.1:<http_port>`.
    pub fn new(api: MailApi, http_port: u16) -> Result<Self> {
        match api {
            MailApi::MailpitV1 => Ok(Self::MailpitV1(MailpitV1::new(&format!(
                "http://127.0.0.1:{http_port}"
            ))?)),
        }
    }

    pub async fn info(&self) -> Result<Counts> {
        match self {
            Self::MailpitV1(b) => b.info().await,
        }
    }

    pub async fn list(&self, params: &MailListParams) -> Result<MailListDto> {
        match self {
            Self::MailpitV1(b) => b.list(params).await,
        }
    }

    pub async fn show(&self, id: &str, keep_unread: bool) -> Result<MailMessageDto> {
        match self {
            Self::MailpitV1(b) => b.show(id, keep_unread).await,
        }
    }

    pub async fn mark_read(&self, ids: &[String], read: bool) -> Result<()> {
        match self {
            Self::MailpitV1(b) => b.mark_read(ids, read).await,
        }
    }

    pub async fn delete(&self, ids: &[String]) -> Result<()> {
        match self {
            Self::MailpitV1(b) => b.delete(ids).await,
        }
    }

    pub async fn clear(&self) -> Result<()> {
        match self {
            Self::MailpitV1(b) => b.clear().await,
        }
    }

    pub async fn attachment(&self, id: &str, part_id: &str) -> Result<MailAttachmentContentDto> {
        match self {
            Self::MailpitV1(b) => b.attachment(id, part_id).await,
        }
    }

    /// The backend's web UI.
    pub fn ui_url(&self) -> String {
        match self {
            Self::MailpitV1(b) => b.ui_url(),
        }
    }
}

/// Mailpit REST API v1.
#[derive(Debug, Clone)]
pub struct MailpitV1 {
    http: reqwest::Client,
    /// `http://127.0.0.1:8025/api/v1/`.
    api: Url,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InfoJson {
    #[serde(default)]
    messages: u32,
    #[serde(default)]
    unread: u32,
}

#[derive(Deserialize, Default)]
struct AddressJson {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Address", default)]
    address: String,
}

impl AddressJson {
    fn display(&self) -> String {
        if self.name.trim().is_empty() {
            self.address.clone()
        } else {
            format!("{} <{}>", self.name.trim(), self.address)
        }
    }
}

#[derive(Deserialize)]
struct ListJson {
    #[serde(default)]
    total: u32,
    #[serde(default)]
    unread: u32,
    #[serde(default)]
    messages_count: Option<u32>,
    #[serde(default)]
    messages: Vec<ListItemJson>,
}

#[derive(Deserialize)]
struct ListItemJson {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Read", default)]
    read: bool,
    #[serde(rename = "From", default)]
    from: Option<AddressJson>,
    #[serde(rename = "To", default)]
    to: Vec<AddressJson>,
    #[serde(rename = "Subject", default)]
    subject: String,
    #[serde(rename = "Created", default)]
    created: Option<String>,
    #[serde(rename = "Size", default)]
    size: u64,
    #[serde(rename = "Attachments", default)]
    attachments: u32,
}

#[derive(Deserialize)]
struct PartJson {
    #[serde(rename = "PartID", default)]
    part_id: String,
    #[serde(rename = "FileName", default)]
    file_name: String,
    #[serde(rename = "ContentType", default)]
    content_type: String,
    #[serde(rename = "ContentID", default)]
    content_id: String,
    #[serde(rename = "Size", default)]
    size: u64,
}

#[derive(Deserialize)]
struct DetailJson {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "From", default)]
    from: Option<AddressJson>,
    #[serde(rename = "To", default)]
    to: Vec<AddressJson>,
    #[serde(rename = "Cc", default)]
    cc: Vec<AddressJson>,
    #[serde(rename = "Bcc", default)]
    bcc: Vec<AddressJson>,
    #[serde(rename = "ReplyTo", default)]
    reply_to: Vec<AddressJson>,
    #[serde(rename = "Subject", default)]
    subject: String,
    #[serde(rename = "Date", default)]
    date: Option<String>,
    #[serde(rename = "Text", default)]
    text: String,
    #[serde(rename = "HTML", default)]
    html: String,
    #[serde(rename = "Size", default)]
    size: u64,
    #[serde(rename = "Attachments", default)]
    attachments: Vec<PartJson>,
    #[serde(rename = "Inline", default)]
    inline: Vec<PartJson>,
}

fn timestamp(text: Option<&str>) -> Timestamp {
    text.and_then(|t| t.parse().ok())
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

fn addresses(list: &[AddressJson]) -> Vec<String> {
    list.iter().map(AddressJson::display).collect()
}

/// Ids of messages and parts go into URL segments: letters, digits, `-` and `_` only.
fn check_id(what: &str, id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(format!("invalid {what} {id:?}")))
    }
}

fn check_ids(ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Err(ForgeError::InvalidInput("no message ids given".into()));
    }
    ids.iter().try_for_each(|id| check_id("message id", id))
}

fn net_error(err: &reqwest::Error) -> ForgeError {
    if err.is_timeout() {
        ForgeError::Timeout {
            operation: "the mail backend".into(),
            after: REQUEST_TIMEOUT,
        }
    } else {
        ForgeError::Conflict(format!("the mail backend is not answering: {err}"))
    }
}

/// `filename` of a `Content-Disposition` header.
fn disposition_file_name(header: &str) -> Option<String> {
    let rest = header.split("filename=").nth(1)?;
    let name = rest.split(';').next()?.trim().trim_matches('"');
    (!name.is_empty()).then(|| name.to_owned())
}

impl MailpitV1 {
    /// Adapter for the server at `base` (`http://127.0.0.1:8025`).
    pub fn new(base: &str) -> Result<Self> {
        let api = Url::parse(&format!("{}/api/v1/", base.trim_end_matches('/')))
            .map_err(|e| ForgeError::Internal(format!("mail backend url: {e}")))?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ForgeError::Internal(format!("building the mail client: {e}")))?;
        Ok(Self { http, api })
    }

    pub fn ui_url(&self) -> String {
        let mut url = self.api.clone();
        url.set_path("/");
        url.to_string()
    }

    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Result<Url> {
        let mut url = self.api.clone();
        url.path_segments_mut()
            .map_err(|()| ForgeError::Internal("mail backend url cannot be a base".into()))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }

    async fn send(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let mut request = self.http.request(method, self.url(segments, query)?);
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await.map_err(|e| net_error(&e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        Err(match status.as_u16() {
            404 => ForgeError::NotFound("message not found in the mailbox".into()),
            400 | 422 => {
                ForgeError::InvalidInput(format!("the mail backend refused the request ({status})"))
            }
            _ => ForgeError::Internal(format!("the mail backend answered {status}")),
        })
    }

    async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let response = self.send(method, segments, query, body).await?;
        let bytes = response.bytes().await.map_err(|e| net_error(&e))?;
        serde_json::from_slice(&bytes).map_err(|e| {
            ForgeError::Internal(format!("unexpected answer of the mail backend: {e}"))
        })
    }

    pub async fn info(&self) -> Result<Counts> {
        let info: InfoJson = self.json(Method::GET, &["info"], &[], None).await?;
        Ok(Counts {
            messages: info.messages,
            unread: info.unread,
        })
    }

    pub async fn list(&self, params: &MailListParams) -> Result<MailListDto> {
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = params.offset.unwrap_or(0);
        let mut query = String::new();
        if params.unread {
            query.push_str("is:unread");
        }
        if let Some(text) = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            if !query.is_empty() {
                query.push(' ');
            }
            query.push_str(text);
        }
        let mut pairs = vec![("start", offset.to_string()), ("limit", limit.to_string())];
        let page: ListJson = if query.is_empty() {
            self.json(Method::GET, &["messages"], &pairs, None).await?
        } else {
            pairs.push(("query", query));
            self.json(Method::GET, &["search"], &pairs, None).await?
        };
        let total = page.messages_count.unwrap_or(page.total);
        let messages = page
            .messages
            .into_iter()
            .map(|m| MailSummaryDto {
                id: m.id,
                from: m
                    .from
                    .as_ref()
                    .map(AddressJson::display)
                    .unwrap_or_default(),
                to: addresses(&m.to),
                subject: m.subject,
                received_at: timestamp(m.created.as_deref()),
                read: m.read,
                size_bytes: m.size,
                attachments: m.attachments,
            })
            .collect();
        Ok(MailListDto {
            messages,
            total,
            unread: page.unread,
        })
    }

    /// Ids of the unread messages (at most [`UNREAD_SCAN_MAX`]).
    async fn unread_ids(&self) -> Result<BTreeSet<String>> {
        let mut ids = BTreeSet::new();
        let mut start = 0;
        while start < UNREAD_SCAN_MAX {
            let query = [
                ("query", "is:unread".to_owned()),
                ("start", start.to_string()),
                ("limit", MAX_LIMIT.to_string()),
            ];
            let page: ListJson = self.json(Method::GET, &["search"], &query, None).await?;
            let got = u32::try_from(page.messages.len()).unwrap_or(u32::MAX);
            ids.extend(page.messages.into_iter().map(|m| m.id));
            if got < MAX_LIMIT {
                break;
            }
            start += got;
        }
        Ok(ids)
    }

    pub async fn show(&self, id: &str, keep_unread: bool) -> Result<MailMessageDto> {
        check_id("message id", id)?;
        let was_unread = if keep_unread {
            self.unread_ids().await?.contains(id)
        } else {
            false
        };
        let detail: DetailJson = self.json(Method::GET, &["message", id], &[], None).await?;
        let headers: BTreeMap<String, Vec<String>> = self
            .json(Method::GET, &["message", id, "headers"], &[], None)
            .await
            .unwrap_or_default();
        if was_unread {
            self.mark_read(&[id.to_owned()], false).await?;
        }
        let part = |p: &PartJson, inline: bool| MailAttachmentDto {
            part_id: p.part_id.clone(),
            file_name: p.file_name.clone(),
            content_type: p.content_type.clone(),
            size_bytes: p.size,
            inline,
            content_id: Some(p.content_id.trim_matches(['<', '>']).to_owned())
                .filter(|c| !c.is_empty()),
        };
        let attachments: Vec<MailAttachmentDto> = detail
            .attachments
            .iter()
            .map(|p| part(p, false))
            .chain(detail.inline.iter().map(|p| part(p, true)))
            .collect();
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        Ok(MailMessageDto {
            summary: MailSummaryDto {
                id: detail.id,
                from: detail
                    .from
                    .as_ref()
                    .map(AddressJson::display)
                    .unwrap_or_default(),
                to: addresses(&detail.to),
                subject: detail.subject.clone(),
                received_at: timestamp(detail.date.as_deref()),
                read: !was_unread,
                size_bytes: detail.size,
                attachments: u32::try_from(detail.attachments.len()).unwrap_or(u32::MAX),
            },
            headers,
            text: non_empty(&detail.text),
            html: non_empty(&detail.html),
            cc: addresses(&detail.cc),
            bcc: addresses(&detail.bcc),
            reply_to: addresses(&detail.reply_to),
            attachments,
        })
    }

    pub async fn mark_read(&self, ids: &[String], read: bool) -> Result<()> {
        check_ids(ids)?;
        let body = serde_json::json!({ "IDs": ids, "Read": read });
        self.send(Method::PUT, &["messages"], &[], Some(body))
            .await?;
        Ok(())
    }

    pub async fn delete(&self, ids: &[String]) -> Result<()> {
        check_ids(ids)?;
        let body = serde_json::json!({ "IDs": ids });
        self.send(Method::DELETE, &["messages"], &[], Some(body))
            .await?;
        Ok(())
    }

    pub async fn clear(&self) -> Result<()> {
        self.send(
            Method::DELETE,
            &["messages"],
            &[],
            Some(serde_json::json!({})),
        )
        .await?;
        Ok(())
    }

    pub async fn attachment(&self, id: &str, part_id: &str) -> Result<MailAttachmentContentDto> {
        check_id("message id", id)?;
        check_id("part id", part_id)?;
        let too_big = |size: Option<u64>| {
            let size = size.map_or_else(String::new, |s| format!(" ({s} bytes)"));
            ForgeError::InvalidInput(format!(
                "the attachment is over the {} MiB limit{size}; open it in the Mailpit UI at {}",
                MAIL_ATTACHMENT_MAX_BYTES / (1024 * 1024),
                self.ui_url()
            ))
        };
        let mut response = self
            .send(Method::GET, &["message", id, "part", part_id], &[], None)
            .await?;
        if response
            .content_length()
            .is_some_and(|n| n > MAIL_ATTACHMENT_MAX_BYTES)
        {
            return Err(too_big(response.content_length()));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let file_name = response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .and_then(disposition_file_name)
            .unwrap_or_else(|| part_id.to_owned());
        let mut content = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| net_error(&e))? {
            if content.len() as u64 + chunk.len() as u64 > MAIL_ATTACHMENT_MAX_BYTES {
                return Err(too_big(None));
            }
            content.extend_from_slice(&chunk);
        }
        Ok(MailAttachmentContentDto {
            file_name,
            content_type,
            size_bytes: content.len() as u64,
            content_base64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &content,
            ),
        })
    }
}

// ------------------------------------------------------------------ the hub

/// Last known counts per mail instance and the publication of `mail.changed`.
#[derive(Debug)]
pub struct MailHub {
    /// Lock `daemon.tables` (leaf).
    last: OrderedMutex<BTreeMap<InstanceId, Counts>>,
}

impl Default for MailHub {
    fn default() -> Self {
        Self {
            last: OrderedMutex::new(LockLevel::DAEMON_TABLES, BTreeMap::new()),
        }
    }
}

impl MailHub {
    /// Records the counts of `id`. The first count since the instance started is a silent
    /// baseline; later ones that differ publish `mail.changed` and return its payload.
    pub fn observe(
        &self,
        bus: &EventBus,
        id: InstanceId,
        counts: Counts,
    ) -> Option<MailChangedPayload> {
        let payload = {
            let mut last = self.last.lock();
            match last.insert(id, counts) {
                Some(prev) if prev != counts => Some(MailChangedPayload {
                    v: 1,
                    instance_id: id,
                    messages: counts.messages,
                    unread: counts.unread,
                    new_messages: counts.messages.saturating_sub(prev.messages),
                }),
                _ => None,
            }
        };
        if let Some(payload) = &payload {
            bus.publish(
                EventKind::MailChanged,
                serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
            );
        }
        payload
    }

    /// The instance is not running: its next count is a new baseline.
    pub fn forget(&self, id: InstanceId) {
        self.last.lock().remove(&id);
    }
}

// ------------------------------------------------------------------ resolution

/// `(entry, spec)` of every instance whose manifest declares `spec.mail`, sorted by label.
pub fn mail_entries(daemon: &Daemon) -> Vec<(Entry, MailSpec)> {
    let mut all: Vec<_> = daemon
        .services
        .entries()
        .into_iter()
        .filter_map(|entry| {
            let spec = entry.manifest.as_ref()?.spec.mail.clone()?;
            Some((entry, spec))
        })
        .collect();
    all.sort_by_key(|(e, _)| e.label());
    all
}

/// What a mail call needs about the instance it talks to.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: InstanceId,
    pub label: String,
    pub running: bool,
    pub ports: BTreeMap<String, u16>,
    pub spec: MailSpec,
}

impl Target {
    async fn from_entry(entry: &Entry, spec: &MailSpec) -> Self {
        let running = match &entry.handle {
            Some(handle) => handle
                .status(QUICK_ASK_TIMEOUT)
                .await
                .is_ok_and(|s| matches!(s.state, InstanceState::Running)),
            None => false,
        };
        Self {
            id: entry.row.id,
            label: entry.label(),
            running,
            ports: entry.ports.clone(),
            spec: spec.clone(),
        }
    }

    fn port(&self, name: &str) -> Option<u16> {
        self.ports.get(name).copied()
    }

    fn backend(&self) -> Result<Backend> {
        let port = self.port(&self.spec.http_port).ok_or_else(|| {
            ForgeError::NotFound(format!(
                "{} has no port named {}",
                self.label, self.spec.http_port
            ))
        })?;
        Backend::new(self.spec.api, port)
    }

    /// The backend of a running instance; `conflict` with the command that fixes it
    /// otherwise.
    fn running_backend(&self) -> Result<Backend> {
        if !self.running {
            let label = &self.label;
            return Err(ForgeError::Conflict(format!(
                "{label} is not running; start it with `forge start {label}`"
            )));
        }
        self.backend()
    }
}

/// The mail instance a call talks to: the only one, or the `default` one. `not_found` with
/// the install command when no installed tool declares `spec.mail`.
pub async fn target(daemon: &Daemon) -> Result<Target> {
    let all = mail_entries(daemon);
    let chosen = match all.len() {
        0 => {
            return Err(ForgeError::NotFound(
                "no mail instance (install one: forge tool install mailpit)".into(),
            ));
        }
        1 => &all[0],
        _ => all
            .iter()
            .find(|(e, _)| e.row.name == "default")
            .unwrap_or(&all[0]),
    };
    Ok(Target::from_entry(&chosen.0, &chosen.1).await)
}

/// Runs `fut` within `budget`; the budget covers the whole call.
async fn within<T>(
    budget: Duration,
    what: &str,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(budget, fut).await.unwrap_or_else(|_| {
        Err(ForgeError::Timeout {
            operation: what.to_owned(),
            after: budget,
        })
    })
}

/// Budget of the calls that make one request.
pub const SINGLE_BUDGET: Duration = Duration::from_millis(2300);
/// Budget of the calls that make a few requests (`show`, `attachment`).
pub const MULTI_BUDGET: Duration = Duration::from_secs(8);

fn publish(daemon: &Daemon, target: &Target, counts: Counts) {
    daemon.mail.observe(&daemon.bus, target.id, counts);
}

// ------------------------------------------------------------------ operations

/// `mail.status`.
pub async fn status(daemon: &Daemon) -> Result<MailStatusDto> {
    let target = target(daemon).await?;
    let smtp_port = target.port(&target.spec.smtp_port);
    if !target.running {
        daemon.mail.forget(target.id);
        return Ok(MailStatusDto {
            running: false,
            instance_id: Some(target.id),
            smtp_port,
            ui_url: None,
            messages: 0,
            unread: 0,
        });
    }
    let backend = target.backend()?;
    let counts = within(SINGLE_BUDGET, "mail.status", backend.info()).await?;
    publish(daemon, &target, counts);
    Ok(MailStatusDto {
        running: true,
        instance_id: Some(target.id),
        smtp_port,
        ui_url: Some(backend.ui_url().trim_end_matches('/').to_owned()),
        messages: counts.messages,
        unread: counts.unread,
    })
}

/// `mail.list`.
pub async fn list(daemon: &Daemon, params: MailListParams) -> Result<MailListDto> {
    let target = target(daemon).await?;
    let backend = target.running_backend()?;
    within(SINGLE_BUDGET, "mail.list", backend.list(&params)).await
}

/// `mail.show`; `change_mailbox` is false for a `viewer`.
pub async fn show(
    daemon: &Daemon,
    id: &str,
    mark_read: bool,
    change_mailbox: bool,
) -> Result<MailMessageDto> {
    let target = target(daemon).await?;
    let backend = target.running_backend()?;
    let keep_unread = !(mark_read && change_mailbox);
    let message = within(MULTI_BUDGET, "mail.show", backend.show(id, keep_unread)).await?;
    if !keep_unread {
        refresh(daemon, &target, &backend).await;
    }
    Ok(message)
}

/// `mail.mark_read`.
pub async fn mark_read(daemon: &Daemon, ids: Vec<String>, read: bool) -> Result<()> {
    let target = target(daemon).await?;
    let backend = target.running_backend()?;
    within(
        SINGLE_BUDGET,
        "mail.mark_read",
        backend.mark_read(&ids, read),
    )
    .await?;
    refresh(daemon, &target, &backend).await;
    Ok(())
}

/// `mail.delete`.
pub async fn delete(daemon: &Daemon, ids: Vec<String>, confirm: bool) -> Result<()> {
    if !confirm {
        return Err(ForgeError::InvalidInput(
            "deleting messages is destructive: pass confirm=true (forge mail delete --yes)".into(),
        ));
    }
    let target = target(daemon).await?;
    let backend = target.running_backend()?;
    within(SINGLE_BUDGET, "mail.delete", backend.delete(&ids)).await?;
    refresh(daemon, &target, &backend).await;
    Ok(())
}

/// `mail.clear`.
pub async fn clear(daemon: &Daemon, confirm: bool) -> Result<()> {
    if !confirm {
        return Err(ForgeError::InvalidInput(
            "clearing the mailbox is destructive: pass confirm=true (forge mail clear --yes)"
                .into(),
        ));
    }
    let target = target(daemon).await?;
    let backend = target.running_backend()?;
    within(SINGLE_BUDGET, "mail.clear", backend.clear()).await?;
    refresh(daemon, &target, &backend).await;
    Ok(())
}

/// `mail.attachment`.
pub async fn attachment(
    daemon: &Daemon,
    id: &str,
    part_id: &str,
) -> Result<MailAttachmentContentDto> {
    let target = target(daemon).await?;
    let backend = target.running_backend()?;
    within(
        MULTI_BUDGET,
        "mail.attachment",
        backend.attachment(id, part_id),
    )
    .await
}

/// `mail.env`: the block's variables with the instance's ports.
pub async fn env(daemon: &Daemon) -> Result<BTreeMap<String, String>> {
    let target = target(daemon).await?;
    target
        .spec
        .render_env(&target.ports)
        .map_err(|e| ForgeError::InvalidInput(e.to_string()))
}

/// The UI of the mailbox for `mail.open`; `conflict` while the instance is stopped.
pub async fn ui_url(daemon: &Daemon) -> Result<String> {
    let target = target(daemon).await?;
    Ok(target.running_backend()?.ui_url())
}

/// After a call that changed the mailbox: look at the counts and publish at once.
async fn refresh(daemon: &Daemon, target: &Target, backend: &Backend) {
    if let Ok(counts) = within(SINGLE_BUDGET, "mail counts", backend.info()).await {
        publish(daemon, target, counts);
    }
}

// ------------------------------------------------------------------ the watcher

/// One look at every mail instance: running ones are counted, stopped ones forgotten.
async fn tick(daemon: &Daemon) {
    for (entry, spec) in mail_entries(daemon) {
        let target = Target::from_entry(&entry, &spec).await;
        if !target.running {
            daemon.mail.forget(target.id);
            continue;
        }
        let Ok(backend) = target.backend() else {
            continue;
        };
        match within(SINGLE_BUDGET, "mail watcher", backend.info()).await {
            Ok(counts) => publish(daemon, &target, counts),
            Err(err) => {
                tracing::debug!(instance = %target.label, %err, "mail watcher could not count")
            }
        }
    }
}

/// The watcher: `/info` every [`WATCH_PERIOD`] ±10 % until `stop`.
pub async fn watch_loop(daemon: Daemon, stop: CancellationToken) {
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(jittered(WATCH_PERIOD, 0.1)) => {}
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tick(&daemon) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    /// What the fake Mailpit saw.
    #[derive(Default)]
    struct Seen {
        queries: Vec<(String, BTreeMap<String, String>)>,
        bodies: Vec<(String, Value)>,
    }

    type Shared = Arc<Mutex<Seen>>;

    fn message(id: &str, read: bool, subject: &str) -> Value {
        json!({
            "ID": id, "MessageID": "x", "Read": read,
            "From": {"Name": "Loja", "Address": "loja@example.test"},
            "To": [{"Name": "", "Address": "ana@example.test"}],
            "Subject": subject, "Created": "2026-10-01T12:00:00.5Z",
            "Size": 1234, "Attachments": 1, "Snippet": "..."
        })
    }

    async fn info() -> impl IntoResponse {
        axum::Json(json!({"Version": "v1.32.0", "Messages": 7, "Unread": 3, "Database": "x"}))
    }

    async fn messages(
        State(seen): State<Shared>,
        Query(q): Query<BTreeMap<String, String>>,
    ) -> impl IntoResponse {
        seen.lock().unwrap().queries.push(("messages".into(), q));
        axum::Json(json!({
            "total": 7, "unread": 3, "count": 2, "messages_count": 7, "start": 0,
            "messages": [message("aaa", false, "Pedido 1"), message("bbb", true, "Pedido 2")]
        }))
    }

    async fn search(
        State(seen): State<Shared>,
        Query(q): Query<BTreeMap<String, String>>,
    ) -> impl IntoResponse {
        seen.lock().unwrap().queries.push(("search".into(), q));
        axum::Json(json!({
            "total": 7, "unread": 3, "count": 1, "messages_count": 1, "start": 0,
            "messages": [message("aaa", false, "Pedido 1")]
        }))
    }

    async fn detail(Path(id): Path<String>) -> impl IntoResponse {
        if id == "missing" {
            return StatusCode::NOT_FOUND.into_response();
        }
        axum::Json(json!({
            "ID": id,
            "From": {"Name": "", "Address": "loja@example.test"},
            "To": [{"Name": "Ana", "Address": "ana@example.test"}],
            "Cc": [{"Name": "", "Address": "cc@example.test"}],
            "Bcc": [], "ReplyTo": [{"Name": "", "Address": "r@example.test"}],
            "Subject": "Pedido 1", "Date": "2026-10-01T12:00:00Z",
            "Text": "ola", "HTML": "",
            "Size": 99,
            "Attachments": [{"PartID": "2", "FileName": "a.pdf", "ContentType": "application/pdf", "ContentID": "", "Size": 10}],
            "Inline": [{"PartID": "3", "FileName": "logo.png", "ContentType": "image/png", "ContentID": "<logo@x>", "Size": 5}]
        }))
        .into_response()
    }

    async fn headers() -> impl IntoResponse {
        axum::Json(json!({"Subject": ["Pedido 1"], "Received": ["a", "b"]}))
    }

    async fn part(Path((_id, part)): Path<(String, String)>) -> impl IntoResponse {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/pdf".parse().unwrap());
        h.insert(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"a.pdf\"".parse().unwrap(),
        );
        let body = match part.as_str() {
            "big" => vec![0_u8; usize::try_from(MAIL_ATTACHMENT_MAX_BYTES).unwrap() + 1],
            "exact" => vec![7_u8; usize::try_from(MAIL_ATTACHMENT_MAX_BYTES).unwrap()],
            _ => b"%PDF-1".to_vec(),
        };
        (h, body)
    }

    async fn put_messages(State(seen): State<Shared>, body: String) -> impl IntoResponse {
        seen.lock()
            .unwrap()
            .bodies
            .push(("PUT".into(), serde_json::from_str(&body).unwrap()));
        "OK"
    }

    async fn delete_messages(State(seen): State<Shared>, body: String) -> impl IntoResponse {
        seen.lock()
            .unwrap()
            .bodies
            .push(("DELETE".into(), serde_json::from_str(&body).unwrap()));
        "OK"
    }

    async fn serve() -> (MailpitV1, Shared) {
        let seen = Shared::default();
        let app = Router::new()
            .route("/api/v1/info", get(info))
            .route(
                "/api/v1/messages",
                get(messages).put(put_messages).delete(delete_messages),
            )
            .route("/api/v1/search", get(search))
            .route("/api/v1/message/{id}", get(detail))
            .route("/api/v1/message/{id}/headers", get(headers))
            .route("/api/v1/message/{id}/part/{part}", get(part))
            .with_state(Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (
            MailpitV1::new(&format!("http://127.0.0.1:{port}")).unwrap(),
            seen,
        )
    }

    #[tokio::test]
    async fn info_maps_the_counts() {
        let (mail, _) = serve().await;
        assert_eq!(
            mail.info().await.unwrap(),
            Counts {
                messages: 7,
                unread: 3
            }
        );
    }

    #[tokio::test]
    async fn list_maps_pages_and_queries() {
        let (mail, seen) = serve().await;
        let page = mail.list(&MailListParams::default()).await.unwrap();
        assert_eq!((page.total, page.unread, page.messages.len()), (7, 3, 2));
        let first = &page.messages[0];
        assert_eq!(first.id, "aaa");
        assert_eq!(first.from, "Loja <loja@example.test>");
        assert_eq!(first.to, vec!["ana@example.test"]);
        assert!(!first.read);
        assert_eq!((first.size_bytes, first.attachments), (1234, 1));
        assert_eq!(first.received_at.to_string(), "2026-10-01T12:00:00.5Z");

        // Paging goes to start/limit (limit clamped), a query or `unread` to /search.
        mail.list(&MailListParams {
            limit: Some(10_000),
            offset: Some(20),
            ..Default::default()
        })
        .await
        .unwrap();
        let hit = mail
            .list(&MailListParams {
                unread: true,
                query: Some(" from:loja pedido ".into()),
                limit: Some(5),
                offset: Some(5),
            })
            .await
            .unwrap();
        // Matching count of the search, badge of the whole mailbox.
        assert_eq!((hit.total, hit.unread), (1, 3));
        let seen = seen.lock().unwrap();
        let (path, q) = &seen.queries[1];
        assert_eq!(path, "messages");
        assert_eq!((q["start"].as_str(), q["limit"].as_str()), ("20", "500"));
        let (path, q) = &seen.queries[2];
        assert_eq!(path, "search");
        assert_eq!(q["query"], "is:unread from:loja pedido");
        assert_eq!((q["start"].as_str(), q["limit"].as_str()), ("5", "5"));
    }

    #[tokio::test]
    async fn show_maps_the_message_and_keeps_unread_on_request() {
        let (mail, seen) = serve().await;
        let m = mail.show("aaa", false).await.unwrap();
        assert!(m.summary.read);
        assert_eq!(m.summary.from, "loja@example.test");
        assert_eq!(m.summary.to, vec!["Ana <ana@example.test>"]);
        assert_eq!((m.text.as_deref(), m.html.as_deref()), (Some("ola"), None));
        assert_eq!(m.cc, vec!["cc@example.test"]);
        assert_eq!(m.reply_to, vec!["r@example.test"]);
        assert_eq!(m.headers["Received"], vec!["a", "b"]);
        assert_eq!(m.summary.attachments, 1);
        assert_eq!(m.attachments.len(), 2);
        assert!(!m.attachments[0].inline);
        assert!(m.attachments[1].inline);
        assert_eq!(m.attachments[1].content_id.as_deref(), Some("logo@x"));
        assert!(seen.lock().unwrap().bodies.is_empty());

        // keep_unread: `aaa` is unread in the fake, so it is marked unread again.
        let kept = mail.show("aaa", true).await.unwrap();
        assert!(!kept.summary.read);
        let bodies = seen.lock().unwrap().bodies.clone();
        assert_eq!(
            bodies,
            vec![("PUT".to_owned(), json!({"IDs": ["aaa"], "Read": false}))]
        );
        assert!(matches!(
            mail.show("missing", false).await,
            Err(ForgeError::NotFound(_))
        ));
        assert!(matches!(
            mail.show("../etc", false).await,
            Err(ForgeError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn mutations_send_the_ids() {
        let (mail, seen) = serve().await;
        mail.mark_read(&["a1".into(), "b2".into()], true)
            .await
            .unwrap();
        mail.delete(&["a1".into()]).await.unwrap();
        mail.clear().await.unwrap();
        assert!(mail.delete(&[]).await.is_err());
        let bodies = seen.lock().unwrap().bodies.clone();
        assert_eq!(
            bodies,
            vec![
                ("PUT".to_owned(), json!({"IDs": ["a1", "b2"], "Read": true})),
                ("DELETE".to_owned(), json!({"IDs": ["a1"]})),
                ("DELETE".to_owned(), json!({})),
            ]
        );
    }

    #[tokio::test]
    async fn attachments_are_capped_at_4_mib() {
        let (mail, _) = serve().await;
        let small = mail.attachment("aaa", "2").await.unwrap();
        assert_eq!(small.file_name, "a.pdf");
        assert_eq!(small.content_type, "application/pdf");
        assert_eq!(small.size_bytes, 6);
        assert_eq!(small.content_base64, "JVBERi0x");

        let exact = mail.attachment("aaa", "exact").await.unwrap();
        assert_eq!(exact.size_bytes, MAIL_ATTACHMENT_MAX_BYTES);

        match mail.attachment("aaa", "big").await {
            Err(ForgeError::InvalidInput(msg)) => {
                assert!(msg.contains("Mailpit UI"), "{msg}");
                assert!(msg.contains("4 MiB"), "{msg}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_silent_backend_times_out_in_two_seconds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accepts and never answers.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok(conn) = listener.accept().await {
                held.push(conn);
            }
        });
        let mail = MailpitV1::new(&format!("http://127.0.0.1:{port}")).unwrap();
        let started = std::time::Instant::now();
        let err = mail.info().await.unwrap_err();
        assert!(matches!(err, ForgeError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_millis(2500));
    }

    #[test]
    fn hub_counts_new_messages_and_baselines_silently() {
        let hub = MailHub::default();
        let bus = EventBus::new();
        let id = InstanceId::new();
        let at = |messages, unread| Counts { messages, unread };
        assert!(hub.observe(&bus, id, at(5, 1)).is_none(), "baseline");
        assert!(hub.observe(&bus, id, at(5, 1)).is_none(), "unchanged");
        let up = hub.observe(&bus, id, at(7, 3)).unwrap();
        assert_eq!((up.messages, up.unread, up.new_messages), (7, 3, 2));
        let down = hub.observe(&bus, id, at(2, 0)).unwrap();
        assert_eq!(down.new_messages, 0);
        hub.forget(id);
        assert!(hub.observe(&bus, id, at(9, 9)).is_none(), "new baseline");
    }

    #[test]
    fn file_names_come_from_the_disposition() {
        assert_eq!(
            disposition_file_name("attachment; filename=\"x y.pdf\"; size=1").as_deref(),
            Some("x y.pdf")
        );
        assert_eq!(disposition_file_name("inline"), None);
    }
}
