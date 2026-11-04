//! Native Windows toasts driven from the tray model (M2-E1-T16, docs/07 §7).
//!
//! The tray actor ([`crate::tray`]) lives in the process, not in the window, so toasts work
//! while the window is hidden. It feeds every event it receives to a [`NoticeEngine`]; the
//! engine decides, with no I/O and no clock of its own, whether the event is a toast:
//!
//! * a service crashed **and** came back (`service.crashed` then `service.running` of the same
//!   instance) → [`NoticeKind::ServiceRestarted`];
//! * an update was installed (`tool.updated`) → [`NoticeKind::UpdateInstalled`];
//! * an install finished (`tool.install.completed`, or a `tools.install` job that is done) →
//!   [`NoticeKind::InstallCompleted`]. Installs run by an update/rollback job are covered by
//!   the update toast and stay silent.
//!
//! Rules, in order: events replayed on resume (`seq` ≤ the cursor taken from `hello` when the
//! connection came up) never notify; a type the user muted never notifies; a burst of one
//! type is **grouped**: the first toast shows at once and the rest of the 5 s window is folded
//! into it (leading edge, so a crash is never announced 5 s late and no timer is needed).
//!
//! Muting ("não mostrar de novo") is the `muted_notifications` array of type ids in the GUI
//! prefs (`prefs.json`); the settings screen writes it with `gui_prefs_set`. Strings come from
//! the shared i18n `notifications` namespace. Performance alerts have their hook
//! ([`NoticeEngine::performance_alert`]) but it is off until M5 (ADR-0044); tunnel-ready
//! toasts are M3.
//!
//! Click: [`Tray::notification_clicked`](crate::tray::Tray::notification_clicked) focuses the
//! window and routes to [`NoticeKind::route`]. `tauri-plugin-notification` 2.4 reports no
//! activation callback on desktop, so on Windows the click that reaches the app is the one
//! Windows itself delivers (it launches/activates the app); a native callback would need a
//! plugin that exposes `on_activated`. That is recorded in docs/07 §7.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

use forge_ipc::api::{JobDonePayload, ToolVersionChangePayload};
use forge_ipc::api::{JobStartedPayload, ServicePayload, ToolInstallPayload};

use crate::i18n::{Language, Namespace, translate};
use crate::prefs::PrefsStore;

/// Key of the muted types in the GUI prefs: an array of [`NoticeKind::id`].
pub const MUTED_KEY: &str = "muted_notifications";

/// Events of one type closer than this to the last shown toast are folded into it.
pub const GROUP_WINDOW: Duration = Duration::from_secs(5);

/// Jobs remembered for de-duplication; the sets are cleared when they reach it.
const REMEMBERED_JOBS: usize = 256;

/// Jobs whose installs are reported by the update toast instead.
const UPDATE_JOBS: &[&str] = &["tools.update", "tools.rollback"];

/// A type of toast (what "não mostrar de novo" mutes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NoticeKind {
    /// A service crashed and was restarted.
    ServiceRestarted,
    UpdateInstalled,
    /// An install or download completed.
    InstallCompleted,
    /// Hook only: off (ADR-0044 defers native performance alerts).
    PerformanceAlert,
}

impl NoticeKind {
    pub const ALL: [NoticeKind; 4] = [
        NoticeKind::ServiceRestarted,
        NoticeKind::UpdateInstalled,
        NoticeKind::InstallCompleted,
        NoticeKind::PerformanceAlert,
    ];

    /// Stable id: the `notifications` i18n keys and the `muted_notifications` pref use it.
    pub fn id(self) -> &'static str {
        match self {
            NoticeKind::ServiceRestarted => "service_restarted",
            NoticeKind::UpdateInstalled => "update_installed",
            NoticeKind::InstallCompleted => "install_completed",
            NoticeKind::PerformanceAlert => "performance_alert",
        }
    }

    pub fn parse(id: &str) -> Option<NoticeKind> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// The window route a click on the toast opens.
    pub fn route(self) -> &'static str {
        match self {
            NoticeKind::ServiceRestarted => "/services",
            NoticeKind::UpdateInstalled | NoticeKind::InstallCompleted => "/catalog",
            NoticeKind::PerformanceAlert => "/dashboard",
        }
    }
}

/// What the engine decided to show (language-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    /// `{placeholder}` values of the body.
    pub vars: Vec<(&'static str, String)>,
    /// Use the body without tool/version (a done install job that named nothing).
    pub generic: bool,
}

/// A toast ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
    pub route: &'static str,
}

impl Toast {
    pub fn from_notice(notice: &Notice, lang: Language) -> Toast {
        let id = notice.kind.id();
        let vars: Vec<(&str, &str)> = notice.vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let body_key = if notice.generic {
            format!("toast.{id}.body_job")
        } else {
            format!("toast.{id}.body")
        };
        Toast {
            kind: notice.kind,
            title: translate(
                lang,
                Namespace::Notifications,
                &format!("toast.{id}.title"),
                &[],
            ),
            body: translate(lang, Namespace::Notifications, &body_key, &vars),
            route: notice.kind.route(),
        }
    }
}

/// Shows toasts. [`PluginNotifier`] in the app; a recorder in tests. Must return at once.
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, toast: &Toast);
}

/// Which types the user muted. [`PrefsMutes`] in the app.
pub trait Mutes: Send + Sync + 'static {
    fn is_muted(&self, kind: NoticeKind) -> bool;
}

/// The muted set of a `prefs.json` document (unknown ids are ignored).
pub fn muted_from(prefs: &crate::prefs::GuiPrefs) -> HashSet<NoticeKind> {
    prefs
        .other
        .get(MUTED_KEY)
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .filter_map(NoticeKind::parse)
                .collect()
        })
        .unwrap_or_default()
}

/// [`Mutes`] over the GUI prefs. Never waits behind a save: while one is writing the file the
/// last muted set read is used.
pub struct PrefsMutes {
    store: Arc<PrefsStore>,
    last: Mutex<HashSet<NoticeKind>>,
}

impl PrefsMutes {
    pub fn new(store: Arc<PrefsStore>) -> Self {
        let last = Mutex::new(muted_from(&store.get()));
        Self { store, last }
    }
}

impl Mutes for PrefsMutes {
    fn is_muted(&self, kind: NoticeKind) -> bool {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(prefs) = self.store.try_get() {
            *last = muted_from(&prefs);
        }
        last.contains(&kind)
    }
}

/// The real notifier: `tauri-plugin-notification`, off the actor's thread.
pub struct PluginNotifier<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> PluginNotifier<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> Notifier for PluginNotifier<R> {
    fn notify(&self, toast: &Toast) {
        let app = self.app.clone();
        let (title, body) = (toast.title.clone(), toast.body.clone());
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(err) = app.notification().builder().title(title).body(body).show() {
                tracing::warn!(%err, "native notification not shown");
            }
        });
    }
}

/// What the tray needs to notify: the sink and the mute source.
#[derive(Clone)]
pub struct Notices {
    pub notifier: Arc<dyn Notifier>,
    pub mutes: Arc<dyn Mutes>,
}

/// The decision logic. Pure apart from [`Mutes`]; the caller gives the clock.
pub struct NoticeEngine {
    mutes: Arc<dyn Mutes>,
    /// Events with `seq` ≤ this were replayed on resume.
    cursor: u64,
    /// Instances that crashed and have not run again (`instance_id` → name).
    crashed: HashMap<String, String>,
    /// Jobs whose installs the update toast covers.
    update_jobs: HashSet<String>,
    /// Jobs that already produced an install toast.
    announced_jobs: HashSet<String>,
    /// Start of the open grouping window of each type.
    shown: HashMap<NoticeKind, Instant>,
    performance_alerts: bool,
}

impl NoticeEngine {
    pub fn new(mutes: Arc<dyn Mutes>) -> Self {
        Self {
            mutes,
            cursor: 0,
            crashed: HashMap::new(),
            update_jobs: HashSet::new(),
            announced_jobs: HashSet::new(),
            shown: HashMap::new(),
            performance_alerts: false,
        }
    }

    /// A (new) connection came up: everything up to `seq` is history.
    pub fn set_cursor(&mut self, seq: u64) {
        self.cursor = seq;
        // What happened before the connection says nothing about the new one.
        self.crashed.clear();
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Reduces one event; `Some` when it must be shown now. `seq` is the envelope's.
    pub fn observe(
        &mut self,
        now: Instant,
        seq: Option<u64>,
        kind: &str,
        payload: &Value,
    ) -> Option<Notice> {
        if seq.is_some_and(|seq| seq <= self.cursor) {
            return None;
        }
        let notice = match kind {
            "service.crashed" => {
                if let Some(p) = parse::<ServicePayload>(payload) {
                    self.crashed.insert(p.instance_id.to_string(), p.name);
                }
                None
            }
            "service.running" => {
                let p = parse::<ServicePayload>(payload)?;
                let name = self.crashed.remove(&p.instance_id.to_string())?;
                Some(Notice {
                    kind: NoticeKind::ServiceRestarted,
                    vars: vec![("service", name)],
                    generic: false,
                })
            }
            // The instance gave up or was stopped on purpose: no restart to announce.
            "service.failed" | "service.stopped" | "service.stopping" => {
                if let Some(p) = parse::<ServicePayload>(payload) {
                    self.crashed.remove(&p.instance_id.to_string());
                }
                None
            }
            "job.started" => {
                if let Some(p) = parse::<JobStartedPayload>(payload) {
                    if UPDATE_JOBS.contains(&p.name.as_str()) {
                        remember(&mut self.update_jobs, p.job_id.to_string());
                    }
                }
                None
            }
            "tool.updated" => {
                let p = parse::<ToolVersionChangePayload>(payload)?;
                let version = p.to.split('-').next().unwrap_or(&p.to).to_owned();
                Some(Notice {
                    kind: NoticeKind::UpdateInstalled,
                    vars: vec![("tool", p.tool.to_string()), ("version", version)],
                    generic: false,
                })
            }
            "tool.install.completed" => {
                let p = parse::<ToolInstallPayload>(payload)?;
                if let Some(job) = p.job_id {
                    let job = job.to_string();
                    if self.update_jobs.contains(&job) || self.announced_jobs.contains(&job) {
                        return None;
                    }
                    remember(&mut self.announced_jobs, job);
                }
                Some(Notice {
                    kind: NoticeKind::InstallCompleted,
                    vars: vec![("tool", p.tool.to_string()), ("version", p.version)],
                    generic: false,
                })
            }
            "job.done" => {
                let p = parse::<JobDonePayload>(payload)?;
                if p.name != "tools.install" {
                    return None;
                }
                let job = p.job_id.to_string();
                if self.update_jobs.contains(&job) || self.announced_jobs.contains(&job) {
                    return None;
                }
                remember(&mut self.announced_jobs, job);
                Some(Notice {
                    kind: NoticeKind::InstallCompleted,
                    vars: Vec::new(),
                    generic: true,
                })
            }
            _ => None,
        }?;
        self.admit(now, notice)
    }

    /// Hook for performance alerts. Off: always `None` until the feature is switched on
    /// (deferred to M5, ADR-0044).
    pub fn performance_alert(
        &mut self,
        now: Instant,
        subject: &str,
        detail: &str,
    ) -> Option<Notice> {
        if !self.performance_alerts {
            return None;
        }
        self.admit(
            now,
            Notice {
                kind: NoticeKind::PerformanceAlert,
                vars: vec![
                    ("subject", subject.to_owned()),
                    ("detail", detail.to_owned()),
                ],
                generic: false,
            },
        )
    }

    /// Mute and grouping, the last gate before a toast.
    fn admit(&mut self, now: Instant, notice: Notice) -> Option<Notice> {
        if self.mutes.is_muted(notice.kind) {
            return None;
        }
        if let Some(started) = self.shown.get(&notice.kind) {
            if now.saturating_duration_since(*started) < GROUP_WINDOW {
                return None;
            }
        }
        self.shown.insert(notice.kind, now);
        Some(notice)
    }
}

fn remember(set: &mut HashSet<String>, job: String) {
    if set.len() >= REMEMBERED_JOBS {
        set.clear();
    }
    set.insert(job);
}

fn parse<T: DeserializeOwned>(payload: &Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}
