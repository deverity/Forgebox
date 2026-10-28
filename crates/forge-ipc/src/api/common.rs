//! Types shared by every API group: version, roles, method metadata, references to
//! entities, acknowledgements, errors and stream subscriptions.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use specta::Type;

use forge_core::ids::{InstanceId, SiteId, SubscriptionId};

/// Version of the IPC API spoken by the daemon. Clients refuse a different `major`;
/// `minor` only grows with additive changes (new methods, new optional fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl ApiVersion {
    /// Whether a peer speaking `other` can talk to us (same `major`).
    pub fn is_compatible_with(self, other: ApiVersion) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The API version implemented by this crate.
pub const API_VERSION: ApiVersion = ApiVersion { major: 1, minor: 0 };

/// Authorization profile of a connection (ADR-0009, rule 7; docs/13 §3.3). Ordered:
/// `viewer < operator < admin`. Local connections are always `admin`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read-only access and metrics.
    Viewer,
    /// Start/stop/logs/sites; no tool install/remove, no secrets.
    Operator,
    /// Everything.
    Admin,
}

impl Role {
    /// Whether this role may call a method requiring `min`.
    pub fn allows(self, min: Role) -> bool {
        self >= min
    }

    /// Wire name (`"admin"`...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a method answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodKind {
    /// Answers within ~1 s with its result.
    Call,
    /// Long operation: answers at once with a `JobDto`; progress and result arrive as
    /// `job.*` events (docs/03 §7.4).
    Job,
    /// Opens a stream: answers with a `SubscriptionDto`, then sends notifications.
    Stream,
}

impl MethodKind {
    /// Wire name used in `methods.ts`.
    pub const fn as_str(self) -> &'static str {
        match self {
            MethodKind::Call => "call",
            MethodKind::Job => "job",
            MethodKind::Stream => "stream",
        }
    }
}

/// Static description of an API method. The table [`crate::api::methods::METHODS`] is
/// the single source for the server dispatcher, the typed client and `methods.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodInfo {
    /// `group.action`, e.g. `"sites.add"`.
    pub name: &'static str,
    pub kind: MethodKind,
    /// Lowest role allowed to call it.
    pub min_role: Role,
    /// Depends on the client's OS (opens an editor, a terminal, a browser...): refused on
    /// remote connections (ADR-0009, rule 6).
    pub local_only: bool,
    /// `kind == Job`: answers with a `JobDto` immediately.
    pub long_running: bool,
    /// Changes state: the server caches the answer per `request_id` so a retry returns
    /// the same answer instead of acting twice (ADR-0009, rule 4).
    pub mutating: bool,
    /// The `forge` commands served by this method (docs/09), `; `-separated.
    pub cli: &'static str,
    /// Template of the equivalent `forge` command ([`super::cli_template`], ADR-0037);
    /// `None` when the method has no CLI command.
    pub cli_template: Option<&'static str>,
    /// Name of the params type (in `generated.ts`).
    pub params: &'static str,
    /// Name of the result type (`JobDto` for jobs, `SubscriptionDto` for streams).
    pub result: &'static str,
    /// Jobs: type of `job.done.result`.
    pub job_result: Option<&'static str>,
    /// Streams: notification carrying each item, its type and the lag notification.
    pub notification: Option<&'static str>,
    pub item: Option<&'static str>,
    pub lagged_notification: Option<&'static str>,
}

impl MethodInfo {
    /// Base description; refine with the builder methods below.
    pub const fn new(
        name: &'static str,
        kind: MethodKind,
        min_role: Role,
        cli: &'static str,
        params: &'static str,
        result: &'static str,
    ) -> Self {
        Self {
            name,
            kind,
            min_role,
            local_only: false,
            long_running: matches!(kind, MethodKind::Job),
            mutating: false,
            cli,
            cli_template: None,
            params,
            result,
            job_result: None,
            notification: None,
            item: None,
            lagged_notification: None,
        }
    }

    /// Sets the template of the equivalent `forge` command.
    pub const fn cli_template(mut self, template: &'static str) -> Self {
        self.cli_template = Some(template);
        self
    }

    /// Marks the method as client-OS dependent.
    pub const fn local_only(mut self) -> Self {
        self.local_only = true;
        self
    }

    /// Marks the method as state-changing (idempotency cache by `request_id`).
    pub const fn mutating(mut self) -> Self {
        self.mutating = true;
        self
    }

    /// Jobs: type of the job's result.
    pub const fn job_result(mut self, ty: &'static str) -> Self {
        self.job_result = Some(ty);
        self
    }

    /// Streams: item notification and lag notification names.
    pub const fn notify(mut self, notification: &'static str, lagged: &'static str) -> Self {
        self.notification = Some(notification);
        self.lagged_notification = Some(lagged);
        self
    }

    /// Streams: type of each item.
    pub const fn item(mut self, ty: &'static str) -> Self {
        self.item = Some(ty);
        self
    }

    /// The `forge` command equivalent to calling this method with `params`, as the user
    /// would type it in PowerShell or `cmd.exe` (ADR-0037). `None` when the method has no
    /// template, no alternative of the template applies, or a value has no portable
    /// quoting.
    pub fn render_cli(&self, params: &serde_json::Value) -> Option<String> {
        super::cli_template::render(self.cli_template?, params)
    }

    /// `group` part of the name (`"sites"` for `"sites.add"`).
    pub fn group(&self) -> &'static str {
        self.name.split_once('.').map_or(self.name, |(g, _)| g)
    }

    /// `action` part of the name (`"add"` for `"sites.add"`).
    pub fn action(&self) -> &'static str {
        self.name.split_once('.').map_or("", |(_, a)| a)
    }
}

/// A method of the API, implemented by the marker types of [`crate::api::methods`].
pub trait ApiMethod: Send + Sync + 'static {
    const INFO: MethodInfo;
    type Params: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Result: Serialize + DeserializeOwned + Send + 'static;
}

/// A method that opens a stream of `Item`s.
pub trait StreamMethod: ApiMethod<Result = SubscriptionDto> {
    type Item: Serialize + DeserializeOwned + Send + 'static;
}

/// Stream params that can resume after a `seq` (reconnection replay).
pub trait Resumable {
    fn resume_from(&self) -> Option<u64>;
    fn set_resume_from(&mut self, seq: Option<u64>);
}

/// Params of methods without arguments. Accepts `{}`, `null` or no `params` at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct NoParams {}

/// Plain acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Ack {
    pub ok: bool,
    /// Optional human detail ("daemon stopping").
    pub message: Option<String>,
}

impl Ack {
    pub fn ok() -> Self {
        Self {
            ok: true,
            message: None,
        }
    }

    pub fn with_message(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: Some(message.into()),
        }
    }
}

/// Error carried in JSON-RPC `error.data`: the stable code of
/// `forge_core::error::ForgeError::code` (also `--json` in the CLI) and a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ApiError {
    /// Stable machine code (`not_found`, `conflict`, `forbidden`...). Never renamed.
    pub code: String,
    /// Human readable message (English, like logs; the CLI/GUI localise by `code`).
    pub message: String,
    /// Optional structured details (port owner, offending path...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[specta(type = Option<specta_typescript::Unknown>)]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// CLI exit code of this error (docs/09).
    pub fn exit_code(&self) -> i32 {
        forge_core::error::exit_code_for(&self.code)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<&forge_core::ForgeError> for ApiError {
    fn from(err: &forge_core::ForgeError) -> Self {
        let details = match err {
            forge_core::ForgeError::PortInUse { port, owner } => {
                Some(serde_json::json!({ "port": port, "owner": owner }))
            }
            _ => None,
        };
        Self {
            code: err.code().to_owned(),
            message: err.to_string(),
            details,
        }
    }
}

impl From<forge_core::ForgeError> for ApiError {
    fn from(err: forge_core::ForgeError) -> Self {
        Self::from(&err)
    }
}

macro_rules! string_ref {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_ref!(
    /// A site, by ULID (`SiteId`) or by host (`loja.test`). Responses always carry the
    /// ULID; hosts are accepted for convenience (ADR-0009, rule 2).
    SiteRef
);
string_ref!(
    /// A service instance, by ULID (`InstanceId`), `tool/name` (`mysql/default`) or `tool`
    /// when it has a single instance.
    InstanceRef
);
string_ref!(
    /// Target of a service command: `all`, a tool (`nginx`), an instance
    /// (`mysql/default` or its ULID) or a PHP pool (`php@8.3`).
    ServiceTarget
);
string_ref!(
    /// A tool and an optional version: `php`, `php@8.3` (series → latest patch),
    /// `php@8.3.36` (exact).
    ToolSpec
);

/// What a [`SiteRef`] designates once parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteKey {
    Id(SiteId),
    Host(String),
}

impl SiteRef {
    /// ULID if it parses as one, otherwise a host (lowercased: hosts are always
    /// lowercase, docs/15 §7).
    pub fn key(&self) -> SiteKey {
        match self.0.parse::<SiteId>() {
            Ok(id) => SiteKey::Id(id),
            Err(_) => SiteKey::Host(self.0.to_ascii_lowercase()),
        }
    }
}

/// What an [`InstanceRef`] designates once parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceKey {
    Id(InstanceId),
    /// `tool` and optional instance name.
    Name {
        tool: String,
        name: Option<String>,
    },
}

impl InstanceRef {
    pub fn key(&self) -> InstanceKey {
        if let Ok(id) = self.0.parse::<InstanceId>() {
            return InstanceKey::Id(id);
        }
        match self.0.split_once('/') {
            Some((tool, name)) => InstanceKey::Name {
                tool: tool.to_owned(),
                name: Some(name.to_owned()),
            },
            None => InstanceKey::Name {
                tool: self.0.clone(),
                name: None,
            },
        }
    }
}

impl ToolSpec {
    /// `(tool, version)` split at `@`.
    pub fn parts(&self) -> (&str, Option<&str>) {
        match self.0.split_once('@') {
            Some((tool, version)) => (tool, Some(version)),
            None => (&self.0, None),
        }
    }
}

/// A file on the daemon's machine. `path` is informative (absolute, `/` separators);
/// `fs_path`, when present, is the root-relative path usable with `fs.read`/`fs.write`
/// (ADR-0009, rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FileRefDto {
    /// What the file is for (`main`, `vhost`, `php.ini`, `conf.d/20-xdebug.ini`...).
    pub role: String,
    pub path: String,
    pub fs_path: Option<String>,
    /// Generated by Forgebox (edits only outside `# forgebox:begin/end` blocks survive).
    pub managed: bool,
}

/// Result of one diagnostic check (`forge doctor`, `forge site info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skipped,
}

/// One diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CheckDto {
    /// Stable id (`dns.nrpt`, `tls.ca_trusted`, `port.80`...).
    pub id: String,
    pub status: CheckStatus,
    pub message: String,
    /// Suggested fix (a `forge` command or a short instruction).
    pub fix: Option<String>,
}

/// Answer of every `*.subscribe` method. Notifications of the stream carry
/// `subscription_id`; they start only after this answer (never before).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SubscriptionDto {
    pub subscription_id: SubscriptionId,
    /// Stream name (`events`, `logs`, `metrics`).
    pub stream: String,
    /// Last `seq` of the stream when the subscription was taken (0 if none), captured
    /// atomically with it: every item with a greater `seq` is delivered or reported as
    /// lagged. A client that subscribed without `resume_from` keeps it as its resume
    /// point until the first item arrives (a drop before that must not lose items). A
    /// value below the `resume_from` sent means the stream restarted (daemon state
    /// reset): clients drop their cursor and resynchronise.
    #[specta(type = specta_typescript::Number)]
    pub last_seq: u64,
}

/// Params of `events.unsubscribe`, `logs.unsubscribe`, `metrics.unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UnsubscribeParams {
    pub subscription_id: SubscriptionId,
}

/// Params of the `*.lagged` notifications: the subscriber was too slow and `missed`
/// items were dropped (oldest first, docs/03 §7.3). Resubscribe with `resume_from` or
/// refetch snapshots to resynchronise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LaggedDto {
    pub subscription_id: SubscriptionId,
    #[specta(type = specta_typescript::Number)]
    pub missed: u64,
}

/// Environment variables (`.env` snippets, `forge mail env`, `forge db url`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub struct EnvVarsDto {
    pub vars: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Admin.allows(Role::Operator));
        assert!(Role::Operator.allows(Role::Viewer));
        assert!(!Role::Viewer.allows(Role::Operator));
        assert_eq!(serde_json::to_value(Role::Operator).unwrap(), "operator");
    }

    #[test]
    fn refs_parse() {
        let id = SiteId::new();
        assert_eq!(SiteRef(id.to_string()).key(), SiteKey::Id(id));
        assert_eq!(
            SiteRef::from("Loja.Test").key(),
            SiteKey::Host("loja.test".into())
        );
        assert_eq!(
            InstanceRef::from("mysql/legacy").key(),
            InstanceKey::Name {
                tool: "mysql".into(),
                name: Some("legacy".into())
            }
        );
        assert_eq!(ToolSpec::from("php@8.3").parts(), ("php", Some("8.3")));
        assert_eq!(ToolSpec::from("nginx").parts(), ("nginx", None));
        assert_eq!(
            serde_json::to_value(SiteRef::from("a.test")).unwrap(),
            "a.test"
        );
    }

    #[test]
    fn api_error_from_forge_error_keeps_code() {
        let err = ApiError::from(forge_core::ForgeError::PortInUse {
            port: 80,
            owner: Some("httpd.exe".into()),
        });
        assert_eq!(err.code, "port_in_use");
        assert_eq!(err.exit_code(), 4);
        assert_eq!(err.details.unwrap()["port"], 80);
    }

    #[test]
    fn no_params_accepts_empty_object() {
        let p: NoParams = serde_json::from_str("{}").unwrap();
        assert_eq!(p, NoParams {});
    }

    #[test]
    fn method_info_builder() {
        let info = MethodInfo::new("sites.add", MethodKind::Job, Role::Operator, "x", "P", "R")
            .mutating()
            .local_only();
        assert!(info.long_running && info.mutating && info.local_only);
        assert_eq!((info.group(), info.action()), ("sites", "add"));
    }
}
