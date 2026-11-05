//! Native Windows toasts driven from the tray model (M2-E1-T16, docs/07 §7).
//!
//! The tray actor ([`crate::tray`]) lives in the process, not in the window, so toasts work
//! while the window is hidden. It feeds every event it receives to a [`NoticeEngine`]; the
//! engine decides, with no I/O and no clock of its own, whether the event is a toast:
//!
//! * a service crashed **and** came back (`service.crashed` then `service.running` of the same
//!   instance) → [`NoticeKind::ServiceRestarted`];
//! * an update was installed (`tool.updated`) → [`NoticeKind::UpdateInstalled`];
//! * an install finished (`tool.install.completed`) → [`NoticeKind::InstallCompleted`]. The
//!   install that runs inside a `tool.update`/`tool.rollback` job (the daemon names those jobs
//!   `tool.update <tool> <from> -> <to>` / `tool.rollback <tool> <series>`, also for scheduled
//!   updates) is covered by the update toast and stays silent. The install event carries the
//!   inner `tool.install <tool>@<version>` job's id, so the match is by tool, not by job.
//!   There is no rule on a done `tools.install` job: its inner jobs already emit the event.
//!
//! Rules, in order: events replayed on resume (`seq` ≤ the cursor taken from `hello` when the
//! connection came up) never notify; a type the user muted never notifies; a burst of one
//! type is **grouped** and closed at its trailing edge: the first event opens a 5 s window,
//! and when it ends ONE toast stands for every distinct item in it ("3 services were
//! restarted: nginx, mysql, redis"). A lone event is therefore announced after the window.
//!
//! The replay and the connection state reach the tray through different paths and in no fixed
//! order, so the engine does not trust a stale cursor: when the connection leaves Connected it
//! holds events ([`NoticeEngine::await_cursor`]) and judges them when the new `hello` arrives
//! ([`NoticeEngine::set_cursor`]).
//!
//! Muting ("não mostrar de novo") is the `muted_notifications` array of type ids in the GUI
//! prefs (`prefs.json`); the settings screen writes it with `gui_prefs_set`. Strings come from
//! the shared i18n `notifications` namespace. Performance alerts have their hook
//! ([`NoticeEngine::performance_alert`]) but it is off until M5 (ADR-0044); tunnel-ready
//! toasts are M3.
//!
//! Click: NOT wired to the toast. [`Tray::notification_clicked`](crate::tray::Tray::notification_clicked)
//! focuses the window and routes to [`NoticeKind::route`], but `tauri-plugin-notification` 2.4
//! has no activation callback or launch argument on desktop, so nothing calls it from a real
//! toast yet. Wiring it needs a notifier with an in-process activation callback behind a
//! forge-core trait; deferred (docs/07 §7).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

use forge_ipc::api::ToolVersionChangePayload;
use forge_ipc::api::{JobStartedPayload, ServicePayload, ToolInstallPayload};

use crate::i18n::{Language, Namespace, translate};
use crate::prefs::PrefsStore;

/// Key of the muted types in the GUI prefs: an array of [`NoticeKind::id`].
pub const MUTED_KEY: &str = "muted_notifications";

/// Events of one type closer than this to the last shown toast are folded into it.
pub const GROUP_WINDOW: Duration = Duration::from_secs(5);

/// In-flight update jobs remembered; the map is cleared when it reaches this.
const REMEMBERED_JOBS: usize = 256;

/// Events held while the cursor is unknown; the oldest is dropped beyond it.
const HELD_EVENTS: usize = 512;

/// Items named in a group's list; the rest is "...".
const LISTED_ITEMS: usize = 5;

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

/// What the engine decided to show (language-free). One notice stands for a whole group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    /// `{placeholder}` values of the body: the event's own for one item; `count` and `list`
    /// for a group of several distinct items.
    pub vars: Vec<(&'static str, String)>,
    /// How many distinct items the notice stands for (1: the plain body is used).
    pub count: usize,
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
        let body_key = if notice.count > 1 {
            format!("toast.{id}.body_many")
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

/// What the tray needs to notify: the sink, the mute source and the grouping window.
#[derive(Clone)]
pub struct Notices {
    pub notifier: Arc<dyn Notifier>,
    pub mutes: Arc<dyn Mutes>,
    /// Length of the grouping window ([`GROUP_WINDOW`] in the app).
    pub window: Duration,
}

/// Events held while the connection's cursor is unknown (see [`NoticeEngine::await_cursor`]).
struct Buffered {
    at: Instant,
    seq: Option<u64>,
    kind: String,
    payload: Value,
}

/// One toast-worthy event, before grouping.
struct Candidate {
    kind: NoticeKind,
    vars: Vec<(&'static str, String)>,
    /// What the item is called in a group's list.
    label: String,
}

/// The open grouping window of one type.
struct Group {
    started: Instant,
    items: Vec<Candidate>,
}

/// The decision logic. Pure apart from [`Mutes`]; the caller gives the clock and calls
/// [`NoticeEngine::flush_due`] when [`NoticeEngine::next_deadline`] passes.
pub struct NoticeEngine {
    mutes: Arc<dyn Mutes>,
    window: Duration,
    /// Events with `seq` ≤ this were replayed on resume.
    cursor: u64,
    /// True from "the connection left Connected" until the next `hello` fixes the cursor.
    /// The resume replay and the state change race through different paths, so while the
    /// cursor is unknown events are held, not judged.
    awaiting: bool,
    buffer: VecDeque<Buffered>,
    /// Instances that crashed and have not run again (`instance_id` → name).
    crashed: HashMap<String, String>,
    /// In-flight update/rollback jobs (`job_id` → tool). The install they run inside is
    /// covered by the update toast.
    updating: HashMap<String, String>,
    /// Open grouping windows, closed at their trailing edge.
    groups: HashMap<NoticeKind, Group>,
    performance_alerts: bool,
}

impl NoticeEngine {
    pub fn new(mutes: Arc<dyn Mutes>) -> Self {
        Self::with_window(mutes, GROUP_WINDOW)
    }

    pub fn with_window(mutes: Arc<dyn Mutes>, window: Duration) -> Self {
        Self {
            mutes,
            window,
            cursor: 0,
            awaiting: false,
            buffer: VecDeque::new(),
            crashed: HashMap::new(),
            updating: HashMap::new(),
            groups: HashMap::new(),
            performance_alerts: false,
        }
    }

    /// The connection is gone or not confirmed: hold events until [`Self::set_cursor`].
    pub fn await_cursor(&mut self) {
        if !self.awaiting {
            self.awaiting = true;
            self.buffer.clear();
        }
    }

    pub fn is_awaiting_cursor(&self) -> bool {
        self.awaiting
    }

    /// A (new) connection came up: everything up to `seq` is history. The events held while
    /// the cursor was unknown are judged now, those after `seq` only.
    pub fn set_cursor(&mut self, seq: u64) {
        self.cursor = seq;
        self.awaiting = false;
        // What happened before the connection says nothing about the new one.
        self.crashed.clear();
        self.updating.clear();
        for held in std::mem::take(&mut self.buffer) {
            self.process(held.at, held.seq, &held.kind, &held.payload);
        }
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Takes one event; `seq` is the envelope's. Toasts come out of [`Self::flush_due`].
    pub fn observe(&mut self, now: Instant, seq: Option<u64>, kind: &str, payload: &Value) {
        if self.awaiting {
            if self.buffer.len() >= HELD_EVENTS {
                self.buffer.pop_front();
            }
            self.buffer.push_back(Buffered {
                at: now,
                seq,
                kind: kind.to_owned(),
                payload: payload.clone(),
            });
            return;
        }
        self.process(now, seq, kind, payload);
    }

    fn process(&mut self, now: Instant, seq: Option<u64>, kind: &str, payload: &Value) {
        if seq.is_some_and(|seq| seq <= self.cursor) {
            return;
        }
        if let Some(candidate) = self.reduce(kind, payload) {
            self.admit(now, candidate);
        }
    }

    fn reduce(&mut self, kind: &str, payload: &Value) -> Option<Candidate> {
        match kind {
            "service.crashed" => {
                if let Some(p) = parse::<ServicePayload>(payload) {
                    self.crashed.insert(p.instance_id.to_string(), p.name);
                }
                None
            }
            "service.running" => {
                let p = parse::<ServicePayload>(payload)?;
                let name = self.crashed.remove(&p.instance_id.to_string())?;
                Some(Candidate {
                    kind: NoticeKind::ServiceRestarted,
                    label: name.clone(),
                    vars: vec![("service", name)],
                })
            }
            // The instance gave up or was stopped on purpose: no restart to announce.
            "service.failed" | "service.stopped" | "service.stopping" => {
                if let Some(p) = parse::<ServicePayload>(payload) {
                    self.crashed.remove(&p.instance_id.to_string());
                }
                None
            }
            // The daemon names the job that updates one tool "tool.update <tool> <from> ->
            // <to>" and "tool.rollback <tool> <series>" (scheduled updates run it too, with
            // no "tools.update" around it). The install inside it emits its own
            // `tool.install.completed`; the update toast covers it.
            "job.started" => {
                let p = parse::<JobStartedPayload>(payload)?;
                let mut words = p.name.split_whitespace();
                if matches!(words.next(), Some("tool.update" | "tool.rollback")) {
                    if let Some(tool) = words.next() {
                        if self.updating.len() >= REMEMBERED_JOBS {
                            self.updating.clear();
                        }
                        self.updating.insert(p.job_id.to_string(), tool.to_owned());
                    }
                }
                None
            }
            "job.done" | "job.failed" | "job.cancelled" => {
                if let Some(job) = payload.get("job_id").and_then(Value::as_str) {
                    self.updating.remove(job);
                }
                None
            }
            "tool.updated" => {
                let p = parse::<ToolVersionChangePayload>(payload)?;
                let version = p.to.split('-').next().unwrap_or(&p.to).to_owned();
                Some(Candidate {
                    kind: NoticeKind::UpdateInstalled,
                    label: format!("{} {version}", p.tool),
                    vars: vec![("tool", p.tool.to_string()), ("version", version)],
                })
            }
            "tool.install.completed" => {
                let p = parse::<ToolInstallPayload>(payload)?;
                let tool = p.tool.to_string();
                if self.updating.values().any(|updating| *updating == tool) {
                    return None;
                }
                Some(Candidate {
                    kind: NoticeKind::InstallCompleted,
                    label: format!("{tool} {}", p.version),
                    vars: vec![("tool", tool), ("version", p.version)],
                })
            }
            _ => None,
        }
    }

    /// Hook for performance alerts. Off: does nothing until the feature is switched on
    /// (deferred to M5, ADR-0044).
    pub fn performance_alert(&mut self, now: Instant, subject: &str, detail: &str) {
        if !self.performance_alerts {
            return;
        }
        self.admit(
            now,
            Candidate {
                kind: NoticeKind::PerformanceAlert,
                label: subject.to_owned(),
                vars: vec![
                    ("subject", subject.to_owned()),
                    ("detail", detail.to_owned()),
                ],
            },
        );
    }

    /// Mute and grouping: a muted type is dropped; the rest joins (or opens) its window.
    fn admit(&mut self, now: Instant, candidate: Candidate) {
        if self.mutes.is_muted(candidate.kind) {
            return;
        }
        self.groups
            .entry(candidate.kind)
            .or_insert_with(|| Group {
                started: now,
                items: Vec::new(),
            })
            .items
            .push(candidate);
    }

    /// When the earliest open window closes.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.groups
            .values()
            .map(|group| group.started + self.window)
            .min()
    }

    /// Closes the windows that ended by `now`: one notice per type, standing for every
    /// distinct item of the window (trailing edge, so nothing is dropped from the group).
    pub fn flush_due(&mut self, now: Instant) -> Vec<Notice> {
        let mut due: Vec<NoticeKind> = self
            .groups
            .iter()
            .filter(|(_, group)| now.saturating_duration_since(group.started) >= self.window)
            .map(|(kind, _)| *kind)
            .collect();
        due.sort();
        due.into_iter()
            .filter_map(|kind| {
                let group = self.groups.remove(&kind)?;
                // A mute set while the window was open still applies.
                if self.mutes.is_muted(kind) {
                    return None;
                }
                Some(Self::summarize(kind, group.items))
            })
            .collect()
    }

    fn summarize(kind: NoticeKind, items: Vec<Candidate>) -> Notice {
        let mut seen = HashSet::new();
        let mut distinct: Vec<Candidate> = items
            .into_iter()
            .filter(|item| seen.insert(item.label.clone()))
            .collect();
        let count = distinct.len();
        if count == 1 {
            let only = distinct.remove(0);
            return Notice {
                kind,
                vars: only.vars,
                count,
            };
        }
        let mut list: Vec<&str> = distinct
            .iter()
            .take(LISTED_ITEMS)
            .map(|item| item.label.as_str())
            .collect();
        if count > LISTED_ITEMS {
            list.push("…");
        }
        Notice {
            kind,
            vars: vec![("count", count.to_string()), ("list", list.join(", "))],
            count,
        }
    }
}

fn parse<T: DeserializeOwned>(payload: &Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}
