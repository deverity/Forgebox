//! The app updater of `Forgebox.exe` ([ADR-0038], docs/05 §5).
//!
//! Layers, from the inside out:
//! - [`decide`]: pure activation policy. Release builds are **inactive** unless `tauri.conf.json`
//!   carries a production public key and endpoint (user decision, M4-E1 supplies them). Debug
//!   builds may point the updater at a local server with `FORGEBOX_UPDATER_ENDPOINT` and then
//!   trust the committed dev key (`keys/updater-dev.pub`).
//! - [`UpdateService`]: the state machine (check, apply) over two small traits,
//!   [`UpdateSource`] (latest.json + download + signature + installer) and [`DaemonStop`]. Tests
//!   drive it with fakes.
//! - [`TauriSource`] / [`BridgeStopper`]: the real implementations over `tauri-plugin-updater`
//!   (minisign verification happens inside `download`) and the bridge (`daemon.stop`).
//!
//! Apply flow: download and verify the signature, `daemon.stop {keep_services: false}` awaited
//! with a 15 s timeout (the installer must replace the locked `forgeboxd.exe`), run the passive
//! installer, which relaunches the app; the GUI then auto-starts the daemon. If the stop fails
//! the installer is **not** launched.
//!
//! No std lock is held across an `.await`; the installer runs in `spawn_blocking`.
//!
//! [ADR-0038]: ../../../../docs/adr/0038-gui-autostart-updater.md

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use forge_ipc::ApiError;

use crate::bridge::contexts::ConnectionStatus;
use crate::bridge::{Bridge, CallArgs};
use crate::i18n::{self, Language, Namespace};
use crate::prefs::PrefsStore;

/// Debug-only override of the endpoint (tests, local servers).
pub const ENDPOINT_ENV: &str = "FORGEBOX_UPDATER_ENDPOINT";
/// Dev public key (Tauri format), trusted only together with [`ENDPOINT_ENV`] in debug builds.
pub const DEV_PUBKEY: &str = include_str!("../keys/updater-dev.pub");
/// Preference key (`prefs.json`) holding the channel.
pub const CHANNEL_PREF: &str = "update_channel";
/// How long the daemon gets to stop (until it is actually gone) before the update is
/// abandoned. The daemon's own shutdown budget is 30 s (`--shutdown-timeout`, pools, sites and
/// instances stop inside it), so this leaves 5 s of margin for the ack and the pipe closing.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(35);
/// Pause after the pipe closes so the process finishes exiting and releases `forgeboxd.exe`.
pub const EXIT_SETTLE: Duration = Duration::from_millis(500);
/// Deadline for the manifest request (a server that accepts and never answers must not hang us).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Deadline for the installer download.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// First automatic check after the app starts (never during first paint).
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
/// Period of the automatic checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Event the webview (and the tray) listen to: carries an [`UpdaterSnapshot`].
pub const UPDATER_EVENT: &str = "forge://updater";

/// Release channel the user follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl Channel {
    /// Parses the pref value; anything unknown is `Stable`.
    pub fn parse(value: &str) -> Channel {
        if value.eq_ignore_ascii_case("beta") {
            Channel::Beta
        } else {
            Channel::Stable
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }

    /// Manifest file of the channel on the release server.
    pub fn manifest_file(self) -> &'static str {
        match self {
            Channel::Stable => "latest.json",
            Channel::Beta => "latest-beta.json",
        }
    }
}

/// The channel stored in the preferences.
pub fn channel_from_prefs(prefs: &PrefsStore) -> Channel {
    prefs
        .get()
        .other
        .get(CHANNEL_PREF)
        .and_then(Value::as_str)
        .map_or(Channel::Stable, Channel::parse)
}

/// Expands `{channel}` and `{channel_file}` in an endpoint template.
pub fn endpoint_url(template: &str, channel: Channel) -> Result<Url, UpdaterError> {
    let text = template
        .replace("{channel_file}", channel.manifest_file())
        .replace("{channel}", channel.as_str());
    Url::parse(&text).map_err(|e| UpdaterError::BadManifest(format!("endpoint {text}: {e}")))
}

/// `plugins.updater` of `tauri.conf.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfSettings {
    pub endpoints: Vec<String>,
    pub pubkey: String,
}

impl ConfSettings {
    /// Reads the plugin section (`None` = absent).
    pub fn from_value(section: Option<&Value>) -> Self {
        let Some(section) = section else {
            return Self::default();
        };
        Self {
            endpoints: section
                .get("endpoints")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            pubkey: section
                .get("pubkey")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    }
}

/// Where the updater ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    Active {
        /// Endpoint template (see [`endpoint_url`]).
        endpoint: String,
        /// Public key in Tauri's format.
        pubkey: String,
        /// The endpoint came from [`ENDPOINT_ENV`].
        from_env: bool,
    },
    /// No production key and endpoint (the 0.2.0 state): the UI says updates arrive in 1.0.
    Inactive,
}

/// The activation policy. `env_endpoint` only counts when `debug` (a release build never lets
/// the environment redirect where it takes installers from).
pub fn decide(conf: &ConfSettings, env_endpoint: Option<&str>, debug: bool) -> Activation {
    if debug && let Some(endpoint) = env_endpoint.map(str::trim).filter(|e| !e.is_empty()) {
        let pubkey = if conf.pubkey.trim().is_empty() {
            DEV_PUBKEY.trim().to_owned()
        } else {
            conf.pubkey.trim().to_owned()
        };
        return Activation::Active {
            endpoint: endpoint.to_owned(),
            pubkey,
            from_env: true,
        };
    }
    match conf.endpoints.first() {
        Some(endpoint) if !conf.pubkey.trim().is_empty() && !endpoint.trim().is_empty() => {
            Activation::Active {
                endpoint: endpoint.trim().to_owned(),
                pubkey: conf.pubkey.trim().to_owned(),
                from_env: false,
            }
        }
        _ => Activation::Inactive,
    }
}

/// Why an updater operation failed. Every variant has a localized message (namespace
/// `updater`, keys `errors.*`) and a stable API code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterError {
    Inactive,
    Busy,
    NoUpdate,
    Network(String),
    BadManifest(String),
    BadSignature,
    DaemonStop(String),
    DaemonStopTimeout(u64),
    Install(String),
    Internal(String),
}

impl UpdaterError {
    /// Stable API code (`forge_core::error::ERROR_CODES`).
    pub fn code(&self) -> &'static str {
        match self {
            UpdaterError::Inactive => "not_implemented",
            UpdaterError::Busy => "conflict",
            UpdaterError::NoUpdate => "not_found",
            UpdaterError::Network(_)
            | UpdaterError::BadManifest(_)
            | UpdaterError::DaemonStop(_)
            | UpdaterError::Install(_) => "io",
            UpdaterError::BadSignature => "verification_failed",
            UpdaterError::DaemonStopTimeout(_) => "timeout",
            UpdaterError::Internal(_) => "internal",
        }
    }

    /// Reason key for `details.reason` (ADR-0039).
    pub fn reason(&self) -> &'static str {
        match self {
            UpdaterError::Inactive => "updater_inactive",
            UpdaterError::Busy => "updater_busy",
            UpdaterError::NoUpdate => "updater_no_update",
            UpdaterError::Network(_) => "updater_network",
            UpdaterError::BadManifest(_) => "updater_bad_manifest",
            UpdaterError::BadSignature => "updater_bad_signature",
            UpdaterError::DaemonStop(_) => "updater_daemon_stop",
            UpdaterError::DaemonStopTimeout(_) => "updater_daemon_stop_timeout",
            UpdaterError::Install(_) => "updater_install",
            UpdaterError::Internal(_) => "updater_internal",
        }
    }

    /// The cause in `lang`.
    pub fn message(&self, lang: Language) -> String {
        let (key, detail, seconds): (&str, &str, String) = match self {
            UpdaterError::Inactive => ("errors.inactive", "", String::new()),
            UpdaterError::Busy => ("errors.busy", "", String::new()),
            UpdaterError::NoUpdate => ("errors.no_update", "", String::new()),
            UpdaterError::Network(d) => ("errors.network", d, String::new()),
            UpdaterError::BadManifest(d) => ("errors.bad_manifest", d, String::new()),
            UpdaterError::BadSignature => ("errors.bad_signature", "", String::new()),
            UpdaterError::DaemonStop(d) => ("errors.daemon_stop", d, String::new()),
            UpdaterError::DaemonStopTimeout(s) => ("errors.daemon_stop_timeout", "", s.to_string()),
            UpdaterError::Install(d) => ("errors.install", d, String::new()),
            UpdaterError::Internal(d) => ("errors.internal", d, String::new()),
        };
        i18n::translate(
            lang,
            Namespace::Updater,
            key,
            &[("detail", detail), ("seconds", &seconds)],
        )
    }

    /// Maps a `tauri-plugin-updater` failure. Signature problems are never "network".
    pub fn from_plugin(err: &tauri_plugin_updater::Error) -> UpdaterError {
        use tauri_plugin_updater::Error as E;
        match err {
            E::Minisign(_)
            | E::Base64(_)
            | E::SignatureUtf8(_)
            | E::SignedVersionMismatch { .. }
            | E::MissingSignedVersion => UpdaterError::BadSignature,
            E::Reqwest(_) | E::Network(_) | E::ReleaseNotFound => {
                UpdaterError::Network(err.to_string())
            }
            E::Serialization(_)
            | E::Semver(_)
            | E::UrlParse(_)
            | E::TargetNotFound(_)
            | E::TargetsNotFound(_)
            | E::EmptyEndpoints => UpdaterError::BadManifest(err.to_string()),
            E::Io(_) => UpdaterError::Install(err.to_string()),
            other => UpdaterError::Internal(other.to_string()),
        }
    }
}

impl std::fmt::Display for UpdaterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message(Language::EnUs))
    }
}

impl std::error::Error for UpdaterError {}

impl From<UpdaterError> for ApiError {
    fn from(err: UpdaterError) -> Self {
        ApiError::new(err.code(), err.message(i18n::current_language()))
            .with_reason(err.reason(), json!({}))
    }
}

/// An update the server announced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    /// Changelog (markdown) shown before applying.
    pub notes: Option<String>,
    /// Publication date as the manifest gave it.
    pub date: Option<String>,
}

/// What the app can do about updates, as the service sees it.
pub trait UpdateSource: Send + Sync + 'static {
    /// The verified installer, ready to run.
    type Package: Send + 'static;

    /// Reads the manifest for the user's channel; `Some` only when it is newer than the
    /// running version. Remembers the announced update for [`UpdateSource::download`].
    fn check(&self) -> impl Future<Output = Result<Option<UpdateInfo>, UpdaterError>> + Send;
    /// Downloads the announced update and verifies its signature
    /// ([`UpdaterError::BadSignature`] when it does not match).
    fn download(&self) -> impl Future<Output = Result<Self::Package, UpdaterError>> + Send;
    /// Runs the installer. On Windows this exits the app once the installer is started.
    fn install(&self, package: Self::Package) -> Result<(), UpdaterError>;
}

/// Stops the daemon so the installer can replace its executable.
pub trait DaemonStop: Send + Sync + 'static {
    fn stop(&self) -> impl Future<Output = Result<(), String>> + Send;
}

/// Where an update stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Stopping,
    Installing,
    Error,
}

impl Phase {
    fn busy(self) -> bool {
        matches!(
            self,
            Phase::Checking | Phase::Downloading | Phase::Stopping | Phase::Installing
        )
    }
}

/// The error the UI shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorInfo {
    pub code: String,
    pub reason: String,
    /// Localized when it happened.
    pub message: String,
}

/// State sent to the webview and the tray (`forge://updater`, `updater_status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdaterSnapshot {
    /// `false` in the 0.2.0 release build: no key or endpoint.
    pub active: bool,
    pub current_version: String,
    pub phase: Phase,
    /// The announced update (also while it downloads or after a failed apply).
    pub available: Option<UpdateInfo>,
    /// Unix milliseconds of the last finished check.
    pub last_checked_ms: Option<u64>,
    pub error: Option<ErrorInfo>,
}

struct Inner {
    phase: Phase,
    available: Option<UpdateInfo>,
    last_checked_ms: Option<u64>,
    error: Option<ErrorInfo>,
}

type Notify = Box<dyn Fn(&UpdaterSnapshot) + Send + Sync>;

/// Check and apply over a source and a daemon.
pub struct UpdateService<S: UpdateSource, D: DaemonStop> {
    source: Arc<S>,
    daemon: D,
    active: bool,
    current_version: String,
    stop_timeout: Duration,
    state: Mutex<Inner>,
    notify: Notify,
}

impl<S: UpdateSource, D: DaemonStop> UpdateService<S, D> {
    pub fn new(
        source: Arc<S>,
        daemon: D,
        active: bool,
        current_version: impl Into<String>,
        notify: Notify,
    ) -> Self {
        Self {
            source,
            daemon,
            active,
            current_version: current_version.into(),
            stop_timeout: STOP_TIMEOUT,
            state: Mutex::new(Inner {
                phase: Phase::Idle,
                available: None,
                last_checked_ms: None,
                error: None,
            }),
            notify,
        }
    }

    /// Overrides the daemon stop deadline (tests).
    #[must_use]
    pub fn with_stop_timeout(mut self, timeout: Duration) -> Self {
        self.stop_timeout = timeout;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn snapshot(&self) -> UpdaterSnapshot {
        let inner = self.lock();
        UpdaterSnapshot {
            active: self.active,
            current_version: self.current_version.clone(),
            phase: inner.phase,
            available: inner.available.clone(),
            last_checked_ms: inner.last_checked_ms,
            error: inner.error.clone(),
        }
    }

    /// Applies `change` and tells the listeners (outside the lock).
    fn update(&self, change: impl FnOnce(&mut Inner)) {
        change(&mut self.lock());
        (self.notify)(&self.snapshot());
    }

    fn fail(&self, err: &UpdaterError) {
        let info = ErrorInfo {
            code: err.code().to_owned(),
            reason: err.reason().to_owned(),
            message: err.message(i18n::current_language()),
        };
        self.update(|s| {
            s.phase = Phase::Error;
            s.error = Some(info);
        });
    }

    /// Asks the server for a newer version.
    pub async fn check(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        if !self.active {
            return Err(UpdaterError::Inactive);
        }
        {
            let mut inner = self.lock();
            if inner.phase.busy() {
                return Err(UpdaterError::Busy);
            }
            inner.phase = Phase::Checking;
            inner.error = None;
        }
        (self.notify)(&self.snapshot());

        let found = self.source.check().await;
        let now = now_ms();
        match found {
            Ok(info) => {
                let found = info.clone();
                self.update(|s| {
                    s.last_checked_ms = Some(now);
                    s.phase = if found.is_some() {
                        Phase::Available
                    } else {
                        Phase::UpToDate
                    };
                    s.available = found;
                });
                Ok(info)
            }
            Err(err) => {
                self.update(|s| s.last_checked_ms = Some(now));
                self.fail(&err);
                Err(err)
            }
        }
    }

    /// Downloads, verifies, stops the daemon and starts the installer. The caller already
    /// showed the changelog and got the user's confirmation.
    pub async fn apply(&self) -> Result<(), UpdaterError> {
        if !self.active {
            return Err(UpdaterError::Inactive);
        }
        {
            let mut inner = self.lock();
            if inner.phase.busy() {
                return Err(UpdaterError::Busy);
            }
            if inner.available.is_none() {
                return Err(UpdaterError::NoUpdate);
            }
            inner.phase = Phase::Downloading;
            inner.error = None;
        }
        (self.notify)(&self.snapshot());

        let package = match self.source.download().await {
            Ok(package) => package,
            Err(err) => {
                self.fail(&err);
                return Err(err);
            }
        };

        self.update(|s| s.phase = Phase::Stopping);
        let stopped = tokio::time::timeout(self.stop_timeout, self.daemon.stop()).await;
        let stop_error = match stopped {
            Ok(Ok(())) => None,
            Ok(Err(detail)) => Some(UpdaterError::DaemonStop(detail)),
            Err(_) => Some(UpdaterError::DaemonStopTimeout(self.stop_timeout.as_secs())),
        };
        if let Some(err) = stop_error {
            // The installer must not run over a live daemon.
            self.fail(&err);
            return Err(err);
        }

        self.update(|s| s.phase = Phase::Installing);
        let source = self.source.clone();
        let installed = tokio::task::spawn_blocking(move || source.install(package))
            .await
            .map_err(|e| UpdaterError::Internal(format!("installer task: {e}")))
            .and_then(|r| r);
        if let Err(err) = installed {
            self.fail(&err);
            return Err(err);
        }
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Checks `first` after the start and then every `every`, until cancelled. A failed or
/// refused check is recorded in the service state and the loop goes on.
pub async fn run_schedule<S: UpdateSource, D: DaemonStop>(
    service: Arc<UpdateService<S, D>>,
    first: Duration,
    every: Duration,
    cancel: CancellationToken,
) {
    let mut wait = first;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        if let Err(err) = service.check().await {
            tracing::debug!(%err, "scheduled update check did not finish");
        }
        wait = every;
    }
}

/// The real source: `tauri-plugin-updater` against the configured endpoint.
pub struct TauriSource<R: Runtime> {
    app: AppHandle<R>,
    endpoint: String,
    channel: Box<dyn Fn() -> Channel + Send + Sync>,
    pending: Mutex<Option<Update>>,
    check_timeout: Duration,
    download_timeout: Duration,
}

/// A downloaded and verified installer.
pub struct Downloaded {
    update: Update,
    bytes: Vec<u8>,
}

impl<R: Runtime> TauriSource<R> {
    pub fn new(
        app: AppHandle<R>,
        endpoint: String,
        channel: impl Fn() -> Channel + Send + Sync + 'static,
    ) -> Self {
        Self {
            app,
            endpoint,
            channel: Box::new(channel),
            pending: Mutex::new(None),
            check_timeout: CHECK_TIMEOUT,
            download_timeout: DOWNLOAD_TIMEOUT,
        }
    }

    /// Overrides the check and download deadlines (tests).
    #[must_use]
    pub fn with_timeouts(mut self, check: Duration, download: Duration) -> Self {
        self.check_timeout = check;
        self.download_timeout = download;
        self
    }

    fn pending(&self) -> Option<Update> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl<R: Runtime> UpdateSource for TauriSource<R> {
    type Package = Downloaded;

    async fn check(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        let url = endpoint_url(&self.endpoint, (self.channel)())?;
        let updater = self
            .app
            .updater_builder()
            .timeout(self.check_timeout)
            .endpoints(vec![url])
            .map_err(|e| UpdaterError::from_plugin(&e))?
            .build()
            .map_err(|e| UpdaterError::from_plugin(&e))?;
        // The builder timeout covers the HTTP request; the outer one also covers a stalled body.
        let found = tokio::time::timeout(self.check_timeout, updater.check())
            .await
            .map_err(|_| UpdaterError::Network("update check timed out".to_owned()))?
            .map_err(|e| UpdaterError::from_plugin(&e))?;
        let info = found.as_ref().map(|u| UpdateInfo {
            version: u.version.clone(),
            notes: u.body.clone(),
            date: u.date.map(|d| d.to_string()),
        });
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = found;
        Ok(info)
    }

    async fn download(&self) -> Result<Downloaded, UpdaterError> {
        let mut update = self.pending().ok_or(UpdaterError::NoUpdate)?;
        update.timeout = Some(self.download_timeout);
        let bytes = tokio::time::timeout(self.download_timeout, update.download(|_, _| {}, || {}))
            .await
            .map_err(|_| UpdaterError::Network("update download timed out".to_owned()))?
            .map_err(|e| UpdaterError::from_plugin(&e))?;
        Ok(Downloaded { update, bytes })
    }

    fn install(&self, package: Downloaded) -> Result<(), UpdaterError> {
        package
            .update
            .install(&package.bytes)
            .map_err(|e| UpdaterError::Install(e.to_string()))
    }
}

/// One event of the daemon connection, as the stopper needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEvent {
    Status(ConnectionStatus),
    /// Events were missed: ask [`DaemonLink::status`] again.
    Lagged,
    /// The stream ended (the app is shutting down).
    Closed,
}

/// The connection state of the daemon, one event at a time.
pub trait LinkEvents: Send + 'static {
    fn next(&mut self) -> impl Future<Output = LinkEvent> + Send;
}

/// The small surface [`LinkedStopper`] needs from the bridge: the stop call and the state
/// stream. A trait so the wait-until-gone logic is testable without a daemon.
pub trait DaemonLink: Send + Sync + 'static {
    type Events: LinkEvents;
    /// Subscribes to state changes (call before [`DaemonLink::request_stop`]).
    fn subscribe(&self) -> Self::Events;
    fn status(&self) -> impl Future<Output = Result<ConnectionStatus, String>> + Send;
    /// `daemon.stop {keep_services:false}`; returns when the daemon acknowledged (it has NOT
    /// exited yet).
    fn request_stop(&self) -> impl Future<Output = Result<(), String>> + Send;
}

impl<L: DaemonLink> DaemonLink for Arc<L> {
    type Events = L::Events;
    fn subscribe(&self) -> L::Events {
        (**self).subscribe()
    }
    fn status(&self) -> impl Future<Output = Result<ConnectionStatus, String>> + Send {
        (**self).status()
    }
    fn request_stop(&self) -> impl Future<Output = Result<(), String>> + Send {
        (**self).request_stop()
    }
}

/// Stops the daemon and waits until it is actually gone, not just until it acked: the daemon
/// answers `daemon.stop` at once and then spends up to its 30 s budget stopping pools, sites
/// and instances before it closes the pipe and exits. The caller bounds the whole call with
/// the service's stop timeout.
pub struct LinkedStopper<L: DaemonLink> {
    link: L,
    settle: Duration,
}

impl<L: DaemonLink> LinkedStopper<L> {
    pub fn new(link: L, settle: Duration) -> Self {
        Self { link, settle }
    }
}

impl<L: DaemonLink> DaemonStop for LinkedStopper<L> {
    async fn stop(&self) -> Result<(), String> {
        // Subscribe first so a disconnect right after the ack cannot be missed.
        let mut events = self.link.subscribe();
        let mut status = self.link.status().await?;
        // A reconnecting daemon may still be alive: wait for a definite state instead of
        // taking the failed call for "stopped".
        while matches!(
            status,
            ConnectionStatus::Reconnecting
                | ConnectionStatus::Connecting
                | ConnectionStatus::Starting
        ) {
            status = match events.next().await {
                LinkEvent::Status(s) => s,
                LinkEvent::Lagged => self.link.status().await?,
                LinkEvent::Closed => return Err("connection events ended".to_owned()),
            };
        }
        if status != ConnectionStatus::Connected {
            // Idle, Stopped, Uninitialized, Closed: no daemon to stop.
            return Ok(());
        }
        self.link.request_stop().await?;
        // Acked. The pipe closes (the context leaves `Connected`) only at the end of the
        // daemon's shutdown, right before the process exits.
        loop {
            let current = match events.next().await {
                LinkEvent::Status(s) => s,
                LinkEvent::Lagged => self.link.status().await?,
                LinkEvent::Closed => return Err("connection events ended".to_owned()),
            };
            if current != ConnectionStatus::Connected {
                break;
            }
        }
        tokio::time::sleep(self.settle).await;
        Ok(())
    }
}

/// The `local` context of the app's bridge.
pub struct BridgeLink {
    app: AppHandle<Wry>,
}

pub struct BridgeEvents {
    rx: broadcast::Receiver<crate::bridge::contexts::ContextStateDto>,
}

impl LinkEvents for BridgeEvents {
    async fn next(&mut self) -> LinkEvent {
        loop {
            match self.rx.recv().await {
                Ok(state) if state.context == "local" => {
                    return LinkEvent::Status(state.status);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return LinkEvent::Lagged,
                Err(broadcast::error::RecvError::Closed) => return LinkEvent::Closed,
            }
        }
    }
}

impl DaemonLink for BridgeLink {
    type Events = BridgeEvents;

    fn subscribe(&self) -> BridgeEvents {
        BridgeEvents {
            rx: self.app.state::<Bridge>().watch_states(),
        }
    }

    async fn status(&self) -> Result<ConnectionStatus, String> {
        let state = self
            .app
            .state::<Bridge>()
            .state("local")
            .await
            .map_err(|e| e.message)?;
        Ok(state.status)
    }

    async fn request_stop(&self) -> Result<(), String> {
        self.app
            .state::<Bridge>()
            .call(CallArgs {
                context: "local".to_owned(),
                method: "daemon.stop".to_owned(),
                params: Some(json!({ "keep_services": false })),
                request_id: None,
                timeout_ms: Some(u64::try_from(STOP_TIMEOUT.as_millis()).unwrap_or(35_000)),
            })
            .await
            .map(|_| ())
            .map_err(|e| e.message)
    }
}

/// The real daemon stop.
pub type BridgeStopper = LinkedStopper<BridgeLink>;

pub fn bridge_stopper(app: AppHandle<Wry>) -> BridgeStopper {
    LinkedStopper::new(BridgeLink { app }, EXIT_SETTLE)
}

/// The updater as the app manages it.
pub type AppUpdater = UpdateService<TauriSource<Wry>, BridgeStopper>;

/// Builds the service for the running app. `activation` comes from [`decide`]; the updater
/// plugin must already be registered when it is `Active`.
pub fn build_service(
    app: &AppHandle<Wry>,
    activation: &Activation,
    prefs: Arc<PrefsStore>,
) -> Arc<AppUpdater> {
    let (endpoint, active) = match activation {
        Activation::Active { endpoint, .. } => (endpoint.clone(), true),
        Activation::Inactive => (String::new(), false),
    };
    let source = Arc::new(TauriSource::new(app.clone(), endpoint, move || {
        channel_from_prefs(&prefs)
    }));
    let emitter = app.clone();
    Arc::new(UpdateService::new(
        source,
        bridge_stopper(app.clone()),
        active,
        env!("CARGO_PKG_VERSION"),
        Box::new(move |snapshot| {
            if let Err(err) = emitter.emit(UPDATER_EVENT, snapshot) {
                tracing::debug!(%err, "updater event not delivered");
            }
        }),
    ))
}

/// Starts the periodic check (first after [`FIRST_CHECK_DELAY`], then every 24 h). Does
/// nothing for an inactive service.
pub fn spawn_schedule(updater: Arc<AppUpdater>) {
    if !updater.snapshot().active {
        return;
    }
    tauri::async_runtime::spawn(run_schedule(
        updater,
        FIRST_CHECK_DELAY,
        CHECK_INTERVAL,
        CancellationToken::new(),
    ));
}

/// The updater state (version, phase, announced update).
#[tauri::command]
pub async fn updater_status(
    updater: State<'_, Arc<AppUpdater>>,
) -> Result<UpdaterSnapshot, ApiError> {
    Ok(updater.snapshot())
}

/// Checks now. `Err(not_implemented)` while the updater is inactive.
#[tauri::command]
pub async fn updater_check(
    updater: State<'_, Arc<AppUpdater>>,
) -> Result<UpdaterSnapshot, ApiError> {
    updater.check().await?;
    Ok(updater.snapshot())
}

/// Applies the announced update (the UI showed the changelog and asked the user first).
/// Does not answer when the installer takes the app down.
#[tauri::command]
pub async fn updater_apply(updater: State<'_, Arc<AppUpdater>>) -> Result<(), ApiError> {
    updater.apply().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    type Log = Arc<Mutex<Vec<&'static str>>>;

    struct FakeSource {
        log: Log,
        announce: Option<UpdateInfo>,
        download_error: Option<UpdaterError>,
        checks: AtomicUsize,
    }

    impl UpdateSource for FakeSource {
        type Package = ();
        async fn check(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            Ok(self.announce.clone())
        }
        async fn download(&self) -> Result<(), UpdaterError> {
            self.log.lock().unwrap().push("download");
            self.download_error.clone().map_or(Ok(()), Err)
        }
        fn install(&self, (): ()) -> Result<(), UpdaterError> {
            self.log.lock().unwrap().push("install");
            Ok(())
        }
    }

    enum StopBehavior {
        Ok,
        Fail,
        Hang,
    }

    struct FakeDaemon {
        log: Log,
        behavior: StopBehavior,
    }

    impl DaemonStop for FakeDaemon {
        async fn stop(&self) -> Result<(), String> {
            self.log.lock().unwrap().push("stop-begin");
            match self.behavior {
                StopBehavior::Ok => {
                    // Yield so "awaited before the installer" is observable.
                    tokio::task::yield_now().await;
                    self.log.lock().unwrap().push("stop-end");
                    Ok(())
                }
                StopBehavior::Fail => Err("access denied".to_owned()),
                StopBehavior::Hang => {
                    std::future::pending::<()>().await;
                    Ok(())
                }
            }
        }
    }

    fn info() -> UpdateInfo {
        UpdateInfo {
            version: "0.3.0".to_owned(),
            notes: Some("- faster".to_owned()),
            date: None,
        }
    }

    fn service(
        behavior: StopBehavior,
        announce: Option<UpdateInfo>,
        download_error: Option<UpdaterError>,
        active: bool,
    ) -> (UpdateService<FakeSource, FakeDaemon>, Log) {
        let log: Log = Arc::default();
        let service = UpdateService::new(
            Arc::new(FakeSource {
                log: log.clone(),
                announce,
                download_error,
                checks: AtomicUsize::new(0),
            }),
            FakeDaemon {
                log: log.clone(),
                behavior,
            },
            active,
            "0.2.0",
            Box::new(|_| {}),
        )
        .with_stop_timeout(Duration::from_millis(100));
        (service, log)
    }

    #[tokio::test]
    async fn daemon_stop_is_awaited_before_the_installer_runs() {
        let (service, log) = service(StopBehavior::Ok, Some(info()), None, true);
        assert_eq!(service.check().await.unwrap(), Some(info()));
        assert_eq!(service.snapshot().phase, Phase::Available);
        service.apply().await.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["download", "stop-begin", "stop-end", "install"]
        );
        assert_eq!(service.snapshot().phase, Phase::Installing);
    }

    #[tokio::test]
    async fn a_failed_stop_never_launches_the_installer() {
        let (service, log) = service(StopBehavior::Fail, Some(info()), None, true);
        service.check().await.unwrap();
        let err = service.apply().await.unwrap_err();
        assert_eq!(err, UpdaterError::DaemonStop("access denied".to_owned()));
        assert!(!log.lock().unwrap().contains(&"install"));
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, Phase::Error);
        assert_eq!(snapshot.error.unwrap().reason, "updater_daemon_stop");
        assert!(snapshot.available.is_some(), "the user can retry");
    }

    #[tokio::test]
    async fn a_daemon_that_does_not_stop_in_time_aborts_the_update() {
        let (service, log) = service(StopBehavior::Hang, Some(info()), None, true);
        service.check().await.unwrap();
        let err = service.apply().await.unwrap_err();
        assert!(matches!(err, UpdaterError::DaemonStopTimeout(_)), "{err:?}");
        assert_eq!(err.code(), "timeout");
        assert!(!log.lock().unwrap().contains(&"install"));
    }

    #[tokio::test]
    async fn a_bad_signature_stops_before_the_daemon_is_touched() {
        let (service, log) = service(
            StopBehavior::Ok,
            Some(info()),
            Some(UpdaterError::BadSignature),
            true,
        );
        service.check().await.unwrap();
        assert_eq!(service.apply().await, Err(UpdaterError::BadSignature));
        assert_eq!(*log.lock().unwrap(), ["download"]);
    }

    #[tokio::test]
    async fn apply_needs_a_checked_update_and_inactive_refuses_everything() {
        let (service, log) = service(StopBehavior::Ok, None, None, true);
        assert_eq!(service.apply().await, Err(UpdaterError::NoUpdate));
        assert_eq!(service.check().await, Ok(None));
        assert_eq!(service.snapshot().phase, Phase::UpToDate);
        assert!(service.snapshot().last_checked_ms.is_some());

        let (inactive, _) = service_inactive();
        assert_eq!(inactive.check().await, Err(UpdaterError::Inactive));
        assert_eq!(inactive.apply().await, Err(UpdaterError::Inactive));
        assert!(!inactive.snapshot().active);
        assert!(log.lock().unwrap().is_empty());
    }

    fn service_inactive() -> (UpdateService<FakeSource, FakeDaemon>, Log) {
        service(StopBehavior::Ok, Some(info()), None, false)
    }

    #[tokio::test]
    async fn the_schedule_checks_first_then_periodically_until_cancelled() {
        let (service, _) = service(StopBehavior::Ok, None, None, true);
        let service = Arc::new(service);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_schedule(
            service.clone(),
            Duration::from_millis(5),
            Duration::from_millis(20),
            cancel.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(120)).await;
        cancel.cancel();
        task.await.unwrap();
        let checks = service.source.checks.load(Ordering::SeqCst);
        assert!(checks >= 3, "{checks} checks");
    }

    #[test]
    fn release_builds_are_inactive_without_a_production_key_and_endpoint() {
        let empty = ConfSettings::default();
        assert_eq!(decide(&empty, None, false), Activation::Inactive);
        // The environment never redirects a release build.
        assert_eq!(
            decide(&empty, Some("http://127.0.0.1:1/latest.json"), false),
            Activation::Inactive
        );
        let half = ConfSettings {
            endpoints: vec!["https://example.test/{channel_file}".to_owned()],
            pubkey: String::new(),
        };
        assert_eq!(decide(&half, None, false), Activation::Inactive);
        let full = ConfSettings {
            endpoints: vec!["https://example.test/{channel_file}".to_owned()],
            pubkey: "KEY".to_owned(),
        };
        assert_eq!(
            decide(&full, None, false),
            Activation::Active {
                endpoint: "https://example.test/{channel_file}".to_owned(),
                pubkey: "KEY".to_owned(),
                from_env: false
            }
        );
    }

    #[test]
    fn debug_builds_honor_the_endpoint_override_with_the_dev_key() {
        let empty = ConfSettings::default();
        match decide(&empty, Some(" http://127.0.0.1:9/latest.json "), true) {
            Activation::Active {
                endpoint,
                pubkey,
                from_env,
            } => {
                assert_eq!(endpoint, "http://127.0.0.1:9/latest.json");
                assert_eq!(pubkey, DEV_PUBKEY.trim());
                assert!(from_env);
            }
            Activation::Inactive => panic!("inactive"),
        }
        assert_eq!(decide(&empty, Some("  "), true), Activation::Inactive);
    }

    #[test]
    fn the_shipped_config_is_inactive_and_the_dev_key_is_a_minisign_key() {
        let conf: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let settings = ConfSettings::from_value(conf.pointer("/plugins/updater"));
        assert_eq!(
            decide(&settings, None, false),
            Activation::Inactive,
            "0.2.0 ships without an app updater (user decision, M4-E1)"
        );
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(DEV_PUBKEY.trim())
            .unwrap();
        assert!(
            String::from_utf8(decoded)
                .unwrap()
                .contains("minisign public key")
        );
    }

    #[test]
    fn channels_choose_the_manifest() {
        assert_eq!(Channel::parse("beta"), Channel::Beta);
        assert_eq!(Channel::parse("anything"), Channel::Stable);
        let template = "https://host.test/dl/{channel_file}";
        assert_eq!(
            endpoint_url(template, Channel::Stable).unwrap().as_str(),
            "https://host.test/dl/latest.json"
        );
        assert_eq!(
            endpoint_url(template, Channel::Beta).unwrap().as_str(),
            "https://host.test/dl/latest-beta.json"
        );
        assert!(endpoint_url("not a url", Channel::Stable).is_err());
    }

    #[test]
    fn errors_have_localized_causes_and_stable_codes() {
        let bad = UpdaterError::BadSignature;
        assert!(bad.message(Language::PtBr).contains("assinatura"));
        assert!(bad.message(Language::EnUs).contains("signature"));
        assert_eq!(bad.code(), "verification_failed");
        let timeout = UpdaterError::DaemonStopTimeout(15);
        assert!(timeout.message(Language::EnUs).contains("15 s"));
        for err in [
            UpdaterError::Inactive,
            UpdaterError::Busy,
            UpdaterError::NoUpdate,
            UpdaterError::Network("x".into()),
            UpdaterError::BadManifest("x".into()),
            UpdaterError::BadSignature,
            UpdaterError::DaemonStop("x".into()),
            UpdaterError::DaemonStopTimeout(1),
            UpdaterError::Install("x".into()),
            UpdaterError::Internal("x".into()),
        ] {
            assert!(
                forge_ipc::base::error::ERROR_CODES.contains(&err.code()),
                "{err:?}"
            );
            assert!(
                !err.message(Language::PtBr).starts_with("errors."),
                "{err:?}"
            );
            let api = ApiError::from(err.clone());
            assert_eq!(api.reason(), Some(err.reason()));
        }
    }

    // --- LinkedStopper: the stop completes only when the daemon is gone -------------------

    struct FakeEvents(broadcast::Receiver<LinkEvent>);

    impl LinkEvents for FakeEvents {
        async fn next(&mut self) -> LinkEvent {
            match self.0.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => LinkEvent::Lagged,
                Err(broadcast::error::RecvError::Closed) => LinkEvent::Closed,
            }
        }
    }

    struct FakeLink {
        log: Log,
        status: Mutex<ConnectionStatus>,
        events: broadcast::Sender<LinkEvent>,
    }

    impl DaemonLink for FakeLink {
        type Events = FakeEvents;
        fn subscribe(&self) -> FakeEvents {
            FakeEvents(self.events.subscribe())
        }
        async fn status(&self) -> Result<ConnectionStatus, String> {
            Ok(*self.status.lock().unwrap())
        }
        async fn request_stop(&self) -> Result<(), String> {
            // The ack arrives at once; the daemon is still running.
            self.log.lock().unwrap().push("ack");
            Ok(())
        }
    }

    fn linked_service(
        initial: ConnectionStatus,
        timeout: Duration,
    ) -> (
        UpdateService<FakeSource, LinkedStopper<Arc<FakeLink>>>,
        Arc<FakeLink>,
        Log,
    ) {
        let log: Log = Arc::default();
        let link = Arc::new(FakeLink {
            log: log.clone(),
            status: Mutex::new(initial),
            events: broadcast::channel(16).0,
        });
        let service = UpdateService::new(
            Arc::new(FakeSource {
                log: log.clone(),
                announce: Some(info()),
                download_error: None,
                checks: AtomicUsize::new(0),
            }),
            LinkedStopper::new(link.clone(), Duration::ZERO),
            true,
            "0.2.0",
            Box::new(|_| {}),
        )
        .with_stop_timeout(timeout);
        (service, link, log)
    }

    #[tokio::test]
    async fn the_installer_starts_only_after_the_daemon_disconnects() {
        let (service, link, log) =
            linked_service(ConnectionStatus::Connected, Duration::from_secs(5));
        service.check().await.unwrap();
        let apply = tokio::spawn(async move { service.apply().await });
        // The ack is in, but the daemon is still stopping its services.
        while !log.lock().unwrap().contains(&"ack") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !log.lock().unwrap().contains(&"install"),
            "installer ran before exit"
        );
        link.events
            .send(LinkEvent::Status(ConnectionStatus::Reconnecting))
            .unwrap();
        apply.await.unwrap().unwrap();
        let log = log.lock().unwrap();
        let ack = log.iter().position(|e| *e == "ack").unwrap();
        let install = log.iter().position(|e| *e == "install").unwrap();
        assert!(ack < install);
    }

    #[tokio::test]
    async fn the_installer_never_starts_when_the_daemon_stays_connected() {
        let (service, _link, log) =
            linked_service(ConnectionStatus::Connected, Duration::from_millis(150));
        service.check().await.unwrap();
        let err = service.apply().await.unwrap_err();
        assert!(matches!(err, UpdaterError::DaemonStopTimeout(_)), "{err:?}");
        assert!(!log.lock().unwrap().contains(&"install"));
    }

    #[tokio::test]
    async fn a_reconnecting_daemon_is_not_taken_for_stopped() {
        let (service, link, log) =
            linked_service(ConnectionStatus::Reconnecting, Duration::from_millis(150));
        service.check().await.unwrap();
        // Still reconnecting at the deadline: no ack, no installer.
        let err = service.apply().await.unwrap_err();
        assert!(matches!(err, UpdaterError::DaemonStopTimeout(_)), "{err:?}");
        assert!(log.lock().unwrap().is_empty() || !log.lock().unwrap().contains(&"install"));

        // It comes back: it gets stopped for real, then the installer runs.
        let (service, link2, log) =
            linked_service(ConnectionStatus::Reconnecting, Duration::from_secs(5));
        drop(link);
        service.check().await.unwrap();
        let apply = tokio::spawn(async move { service.apply().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!log.lock().unwrap().contains(&"ack"));
        link2
            .events
            .send(LinkEvent::Status(ConnectionStatus::Connected))
            .unwrap();
        while !log.lock().unwrap().contains(&"ack") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!log.lock().unwrap().contains(&"install"));
        link2
            .events
            .send(LinkEvent::Status(ConnectionStatus::Stopped))
            .unwrap();
        apply.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn no_daemon_means_nothing_to_wait_for() {
        let (service, _link, log) =
            linked_service(ConnectionStatus::Stopped, Duration::from_secs(5));
        service.check().await.unwrap();
        service.apply().await.unwrap();
        assert!(!log.lock().unwrap().contains(&"ack"));
        assert!(log.lock().unwrap().contains(&"install"));
    }
}
