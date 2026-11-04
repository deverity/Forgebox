//! Native toasts from the tray model (M2-E1-T16, docs/07 §7): the decision engine, the
//! pt-BR/en-US string parity, the prefs mute source, and the tray actor wired to a fake
//! notifier (nothing here shows a real toast).

mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use common::*;
use forge_ipc::API_VERSION;
use forge_ipc::ApiError;
use forge_ipc::api::{HelloDto, Role, TransportKind};
use forge_ipc::base::Timestamp;
use forge_ipc::base::ids::SessionId;
use forgebox_desktop::bridge::{
    AutoStartDto, CallArgs, ConnectionStatus, ContextStateDto, FrameSink, LOCAL_CONTEXT,
    StreamEntry, StreamFrame, SubscribeAnswer,
};
use forgebox_desktop::i18n::Language;
use forgebox_desktop::notifications::{
    GROUP_WINDOW, MUTED_KEY, Mutes, Notice, NoticeEngine, NoticeKind, Notices, Notifier,
    PrefsMutes, Toast, muted_from,
};
use forgebox_desktop::prefs::PrefsStore;
use forgebox_desktop::tray::icons::IconKey;
use forgebox_desktop::tray::{
    QuitChoice, Tray, TrayAppearance, TrayBackend, TrayOptions, TrayRender, TrayView,
};

/// The grouping window of the actor tests (the engine tests use the real 5 s on a fake clock).
const TEST_WINDOW: Duration = Duration::from_millis(400);

const NGINX: &str = "01JAAAAAAAAAAAAAAAAAAAAAA1";
const MYSQL: &str = "01JAAAAAAAAAAAAAAAAAAAAAA2";
const OUTER: &str = "01JEEEEEEEEEEEEEEEEEEEEEE1";
const INNER: &str = "01JEEEEEEEEEEEEEEEEEEEEEE2";
const UPDATE_JOB: &str = "01JEEEEEEEEEEEEEEEEEEEEEE3";
const VERSION: &str = "01JCCCCCCCCCCCCCCCCCCCCCC1";

// --- fixtures -------------------------------------------------------------------------

fn service(id: &str, tool: &str, state: &str) -> Value {
    json!({
        "v": 1, "instance_id": id, "name": format!("{tool}/default"), "tool": tool,
        "state": state, "pid": null, "strategy": null, "exit_code": null, "error": null
    })
}

fn install(tool: &str, version: &str, job: Option<&str>) -> Value {
    json!({
        "v": 1, "job_id": job, "tool": tool, "version": version, "version_id": VERSION,
        "progress": null, "error": null
    })
}

fn updated(tool: &str, to: &str) -> Value {
    json!({ "v": 1, "tool": tool, "from": "8.3.36", "to": to, "version_id": VERSION })
}

fn job(id: &str, name: &str) -> Value {
    json!({ "v": 1, "job_id": id, "name": name })
}

fn job_done(id: &str, name: &str) -> Value {
    json!({ "v": 1, "job_id": id, "name": name, "duration_ms": 10, "result": null })
}

#[derive(Default)]
struct FakeMutes(Mutex<HashSet<NoticeKind>>);

impl FakeMutes {
    fn muting(kinds: &[NoticeKind]) -> Arc<Self> {
        Arc::new(Self(Mutex::new(kinds.iter().copied().collect())))
    }
}

impl Mutes for FakeMutes {
    fn is_muted(&self, kind: NoticeKind) -> bool {
        self.0.lock().unwrap().contains(&kind)
    }
}

fn engine(muted: &[NoticeKind]) -> NoticeEngine {
    NoticeEngine::new(FakeMutes::muting(muted))
}

/// Three services crash and come back within a second.
fn three_pairs() -> Vec<(&'static str, Value)> {
    let ids = [NGINX, MYSQL, "01JAAAAAAAAAAAAAAAAAAAAAA3"];
    ids.iter()
        .enumerate()
        .flat_map(|(i, id)| {
            let tool = ["nginx", "mysql", "redis"][i];
            [
                ("service.crashed", service(id, tool, "crashed")),
                ("service.running", service(id, tool, "running")),
            ]
        })
        .collect()
}

// --- engine ---------------------------------------------------------------------------

/// Feeds `events` (kind, payload) 100 ms apart from `t0`, with consecutive seqs after `seq0`.
fn feed(engine: &mut NoticeEngine, t0: Instant, seq0: u64, events: &[(&str, Value)]) {
    for (i, (kind, payload)) in events.iter().enumerate() {
        engine.observe(
            t0 + Duration::from_millis(100 * i as u64),
            Some(seq0 + 1 + i as u64),
            kind,
            payload,
        );
    }
}

/// The notices of every window that has ended `GROUP_WINDOW` after `t0`.
fn flush(engine: &mut NoticeEngine, t0: Instant) -> Vec<Notice> {
    engine.flush_due(t0 + GROUP_WINDOW + Duration::from_secs(1))
}

fn kinds(notices: &[Notice]) -> Vec<NoticeKind> {
    notices.iter().map(|n| n.kind).collect()
}

fn var<'a>(notice: &'a Notice, name: &str) -> &'a str {
    notice
        .vars
        .iter()
        .find(|(key, _)| *key == name)
        .map_or("", |(_, value)| value.as_str())
}

#[test]
fn three_crash_and_running_pairs_in_five_seconds_make_one_toast_naming_all_three() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    feed(&mut engine, t0, 0, &three_pairs());
    // Trailing edge: nothing before the window ends. It opened with the first restart (the
    // first `service.running`, 100 ms in); a crash alone opens nothing.
    let opened = t0 + Duration::from_millis(100);
    assert!(
        engine
            .flush_due(opened + GROUP_WINDOW - Duration::from_millis(1))
            .is_empty()
    );
    assert_eq!(engine.next_deadline(), Some(opened + GROUP_WINDOW));
    let toasts = engine.flush_due(opened + GROUP_WINDOW);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].kind, NoticeKind::ServiceRestarted);
    assert_eq!(toasts[0].count, 3);
    assert_eq!(var(&toasts[0], "count"), "3");
    assert_eq!(
        var(&toasts[0], "list"),
        "nginx/default, mysql/default, redis/default"
    );
    assert_eq!(engine.next_deadline(), None);
    let text = Toast::from_notice(&toasts[0], Language::PtBr);
    assert_eq!(
        text.body,
        "3 serviços foram reiniciados: nginx/default, mysql/default, redis/default."
    );
    let text = Toast::from_notice(&toasts[0], Language::EnUs);
    assert_eq!(
        text.body,
        "3 services were restarted: nginx/default, mysql/default, redis/default."
    );
}

#[test]
fn a_restart_repeated_in_the_window_is_one_item_and_a_long_list_is_cut() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    let twice = [
        ("service.crashed", service(NGINX, "nginx", "crashed")),
        ("service.running", service(NGINX, "nginx", "running")),
        ("service.crashed", service(NGINX, "nginx", "crashed")),
        ("service.running", service(NGINX, "nginx", "running")),
    ];
    feed(&mut engine, t0, 0, &twice);
    let toasts = flush(&mut engine, t0);
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].count, 1, "the same service twice is one item");
    assert_eq!(var(&toasts[0], "service"), "nginx/default");

    let mut events = Vec::new();
    for i in 0..7 {
        let id = format!("01JAAAAAAAAAAAAAAAAAAAAB{i:02}");
        let tool = format!("svc{i}");
        events.push(("service.crashed", service(&id, &tool, "crashed")));
        events.push(("service.running", service(&id, &tool, "running")));
    }
    feed(&mut engine, t0, 100, &events);
    let toasts = flush(&mut engine, t0);
    assert_eq!(toasts[0].count, 7);
    assert_eq!(
        var(&toasts[0], "list"),
        "svc0/default, svc1/default, svc2/default, svc3/default, svc4/default, …"
    );
}

#[test]
fn a_new_toast_shows_once_the_window_has_passed() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    let pair = |id, tool| {
        [
            ("service.crashed", service(id, tool, "crashed")),
            ("service.running", service(id, tool, "running")),
        ]
    };
    feed(&mut engine, t0, 0, &pair(NGINX, "nginx"));
    // Just inside the window: joins the group.
    let inside = t0 + GROUP_WINDOW - Duration::from_millis(1);
    feed(&mut engine, inside, 10, &pair(MYSQL, "mysql"));
    let first = engine.flush_due(t0 + GROUP_WINDOW + Duration::from_millis(100));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].count, 2);
    // After it: a window of its own.
    let after = t0 + GROUP_WINDOW + Duration::from_millis(1);
    feed(&mut engine, after, 20, &pair(MYSQL, "mysql"));
    let second = engine.flush_due(after + GROUP_WINDOW + Duration::from_millis(100));
    assert_eq!(second.len(), 1);
    assert_eq!(var(&second[0], "service"), "mysql/default");
}

#[test]
fn types_are_grouped_independently() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    let events = [
        ("service.crashed", service(NGINX, "nginx", "crashed")),
        ("service.running", service(NGINX, "nginx", "running")),
        ("tool.updated", updated("php", "8.3.37-nts-vs17-x64")),
        ("tool.install.completed", install("redis", "7.4.1", None)),
    ];
    feed(&mut engine, t0, 0, &events);
    assert_eq!(
        kinds(&flush(&mut engine, t0)),
        [
            NoticeKind::ServiceRestarted,
            NoticeKind::UpdateInstalled,
            NoticeKind::InstallCompleted
        ]
    );
}

#[test]
fn a_muted_type_makes_no_toast_and_the_others_still_do() {
    let mut muted = engine(&[NoticeKind::ServiceRestarted]);
    let t0 = Instant::now();
    feed(&mut muted, t0, 0, &three_pairs());
    feed(
        &mut muted,
        t0,
        50,
        &[("tool.updated", updated("php", "8.3.37-nts-vs17-x64"))],
    );
    assert_eq!(kinds(&flush(&mut muted, t0)), [NoticeKind::UpdateInstalled]);
}

#[test]
fn a_mute_set_while_running_applies_to_the_next_event_and_to_an_open_window() {
    let mutes = FakeMutes::muting(&[]);
    let mut engine = NoticeEngine::new(Arc::clone(&mutes) as Arc<dyn Mutes>);
    let t0 = Instant::now();
    mutes.0.lock().unwrap().insert(NoticeKind::UpdateInstalled);
    let updates = [("tool.updated", updated("php", "8.3.37"))];
    feed(&mut engine, t0, 0, &updates);
    // Muted events do not open a grouping window.
    assert_eq!(engine.next_deadline(), None);
    mutes.0.lock().unwrap().clear();
    feed(&mut engine, t0, 1, &updates);
    assert_eq!(flush(&mut engine, t0).len(), 1);

    // Muting inside an open window silences its toast.
    feed(&mut engine, t0, 10, &updates);
    mutes.0.lock().unwrap().insert(NoticeKind::UpdateInstalled);
    assert!(flush(&mut engine, t0).is_empty());
}

#[test]
fn replayed_events_make_no_toast() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    engine.set_cursor(100);
    // seq <= cursor: all history, including the crash that would pair with a live running.
    feed(&mut engine, t0, 90, &three_pairs()[..2]);
    engine.observe(t0, Some(100), "tool.updated", &updated("php", "8.3.37"));
    // A live running does not pair with a replayed crash either.
    engine.observe(
        t0,
        Some(101),
        "service.running",
        &service(NGINX, "nginx", "running"),
    );
    assert!(flush(&mut engine, t0).is_empty());
    // A live event after the cursor does notify.
    engine.observe(t0, Some(101), "tool.updated", &updated("php", "8.3.37"));
    assert_eq!(
        kinds(&flush(&mut engine, t0)),
        [NoticeKind::UpdateInstalled]
    );
}

#[test]
fn events_held_while_the_cursor_is_unknown_are_judged_by_the_new_hello() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    engine.set_cursor(10);
    // The connection drops; the replay (seq 11..16) arrives before the new hello.
    engine.await_cursor();
    assert!(engine.is_awaiting_cursor());
    feed(&mut engine, t0, 10, &three_pairs());
    assert_eq!(engine.next_deadline(), None, "held, not judged");
    // The new hello says the daemon was at 16: all six events are history.
    engine.set_cursor(16);
    assert!(!engine.is_awaiting_cursor());
    assert!(flush(&mut engine, t0).is_empty());
    // A live event after it is shown.
    engine.observe(t0, Some(17), "tool.updated", &updated("php", "8.3.37"));
    assert_eq!(
        kinds(&flush(&mut engine, t0)),
        [NoticeKind::UpdateInstalled]
    );

    // Events that happened after the hello's cursor but arrived early are not lost.
    engine.await_cursor();
    engine.observe(t0, Some(18), "tool.updated", &updated("nginx", "1.27.1"));
    engine.observe(t0, Some(15), "tool.updated", &updated("old", "1.0.0"));
    engine.set_cursor(17);
    let notices = flush(&mut engine, t0);
    assert_eq!(notices.len(), 1);
    assert_eq!(var(&notices[0], "tool"), "nginx");
}

#[test]
fn the_held_events_are_bounded() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    engine.await_cursor();
    for seq in 1..=5_000u64 {
        engine.observe(
            t0,
            Some(seq),
            "tool.updated",
            &updated(&format!("t{seq}"), "1.0.0"),
        );
    }
    engine.set_cursor(0);
    let notices = flush(&mut engine, t0);
    // Only the newest are kept (the oldest were dropped), and the toast still names a few.
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].count > 1 && notices[0].count <= 512,
        "{notices:?}"
    );
}

#[test]
fn a_crash_without_a_restart_is_not_a_toast() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    let events = [
        ("service.crashed", service(NGINX, "nginx", "crashed")),
        ("service.failed", service(NGINX, "nginx", "failed")),
        ("service.running", service(NGINX, "nginx", "running")),
        // A plain start never announces a restart.
        ("service.running", service(MYSQL, "mysql", "running")),
    ];
    feed(&mut engine, t0, 0, &events);
    assert!(flush(&mut engine, t0).is_empty());
}

#[test]
fn a_new_connection_forgets_pending_crashes() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    engine.observe(
        t0,
        Some(1),
        "service.crashed",
        &service(NGINX, "nginx", "crashed"),
    );
    engine.set_cursor(0);
    engine.observe(
        t0,
        Some(2),
        "service.running",
        &service(NGINX, "nginx", "running"),
    );
    assert!(flush(&mut engine, t0).is_empty());
}

#[test]
fn update_and_install_toasts() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    feed(
        &mut engine,
        t0,
        0,
        &[
            ("tool.updated", updated("php", "8.3.37-nts-vs17-x64")),
            ("tool.install.completed", install("redis", "7.4.1", None)),
        ],
    );
    let notices = flush(&mut engine, t0);
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].kind, NoticeKind::UpdateInstalled);
    assert_eq!(
        notices[0].vars,
        [("tool", "php".to_owned()), ("version", "8.3.37".to_owned())]
    );
    assert_eq!(notices[1].kind, NoticeKind::InstallCompleted);
    assert_eq!(
        notices[1].vars,
        [
            ("tool", "redis".to_owned()),
            ("version", "7.4.1".to_owned())
        ]
    );
}

/// What the daemon emits for `tools.install php@8.3` (api/tools.rs names the outer job
/// "tools.install <specs>"; forge-provider layout.rs runs "tool.install <tool>@<version>"
/// inside it, and `tool.install.completed` carries the INNER job's id).
fn install_sequence() -> Vec<(&'static str, Value)> {
    vec![
        ("job.started", job(OUTER, "tools.install php@8.3")),
        ("job.started", job(INNER, "tool.install php@8.3.37")),
        (
            "tool.install.completed",
            install("php", "8.3.37", Some(INNER)),
        ),
        ("job.done", job_done(INNER, "tool.install php@8.3.37")),
        ("job.done", job_done(OUTER, "tools.install php@8.3")),
    ]
}

#[test]
fn an_install_makes_exactly_one_toast() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    feed(&mut engine, t0, 0, &install_sequence());
    let notices = flush(&mut engine, t0);
    assert_eq!(kinds(&notices), [NoticeKind::InstallCompleted]);
    assert_eq!(var(&notices[0], "tool"), "php");
    assert_eq!(var(&notices[0], "version"), "8.3.37");
}

/// `tools.update` (outer) runs "tool.update <tool> <from> -> <to>", which runs the layout
/// install job, then emits `tool.updated`. A scheduled update has no outer job.
fn update_sequence(outer: bool) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    if outer {
        events.push(("job.started", job(OUTER, "tools.update")));
    }
    events.extend([
        (
            "job.started",
            job(UPDATE_JOB, "tool.update php 8.3.36 -> 8.3.37"),
        ),
        ("job.started", job(INNER, "tool.install php@8.3.37")),
        (
            "tool.install.completed",
            install("php", "8.3.37", Some(INNER)),
        ),
        ("job.done", job_done(INNER, "tool.install php@8.3.37")),
        ("tool.updated", updated("php", "8.3.37-nts-vs17-x64")),
        (
            "job.done",
            job_done(UPDATE_JOB, "tool.update php 8.3.36 -> 8.3.37"),
        ),
    ]);
    if outer {
        events.push(("job.done", job_done(OUTER, "tools.update")));
    }
    events
}

#[test]
fn an_update_makes_only_the_update_toast_manual_or_scheduled() {
    for outer in [true, false] {
        let mut engine = engine(&[]);
        let t0 = Instant::now();
        feed(&mut engine, t0, 0, &update_sequence(outer));
        let notices = flush(&mut engine, t0);
        assert_eq!(kinds(&notices), [NoticeKind::UpdateInstalled], "{outer}");
        assert_eq!(var(&notices[0], "version"), "8.3.37");
    }
}

#[test]
fn a_rollback_does_not_announce_its_install_and_the_flag_clears_when_the_job_ends() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    let rollback = [
        ("job.started", job(UPDATE_JOB, "tool.rollback php 8.3")),
        (
            "tool.install.completed",
            install("php", "8.3.36", Some(INNER)),
        ),
        ("job.failed", job_done(UPDATE_JOB, "tool.rollback php 8.3")),
    ];
    feed(&mut engine, t0, 0, &rollback);
    assert!(flush(&mut engine, t0).is_empty());
    // The job ended (failed): an install of the same tool is a toast again.
    feed(&mut engine, t0, 10, &install_sequence());
    assert_eq!(
        kinds(&flush(&mut engine, t0)),
        [NoticeKind::InstallCompleted]
    );

    // Another tool's install during an update is still announced.
    let mut engine = self::engine(&[]);
    feed(
        &mut engine,
        t0,
        0,
        &[
            (
                "job.started",
                job(UPDATE_JOB, "tool.update php 8.3.36 -> 8.3.37"),
            ),
            ("tool.install.completed", install("redis", "7.4.1", None)),
        ],
    );
    assert_eq!(
        kinds(&flush(&mut engine, t0)),
        [NoticeKind::InstallCompleted]
    );
}

#[test]
fn the_performance_alert_hook_is_off() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    engine.performance_alert(t0, "loja.test", "p99 > 800 ms");
    assert!(flush(&mut engine, t0).is_empty());
}

// --- strings --------------------------------------------------------------------------

fn bundle(lang: &str) -> Value {
    let text = match lang {
        "pt-BR" => include_str!("../../src/i18n/pt-BR/notifications.json"),
        _ => include_str!("../../src/i18n/en-US/notifications.json"),
    };
    serde_json::from_str(text).unwrap()
}

fn placeholders(text: &str) -> Vec<String> {
    let mut found: Vec<String> = text
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name.to_owned()))
        .collect();
    found.sort();
    found
}

#[test]
fn every_type_has_pt_br_and_en_us_strings_with_the_same_placeholders() {
    let (pt, en) = (bundle("pt-BR"), bundle("en-US"));
    for kind in NoticeKind::ALL {
        let id = kind.id();
        let paths = vec![
            format!("/toast/{id}/title"),
            format!("/toast/{id}/body"),
            format!("/toast/{id}/body_many"),
            format!("/types/{id}"),
        ];
        for path in paths {
            let (a, b) = (pt.pointer(&path), en.pointer(&path));
            let (a, b) = (
                a.and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("pt-BR {path}")),
                b.and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("en-US {path}")),
            );
            assert!(!a.is_empty() && !b.is_empty(), "{path}");
            // Only the alert body is the same in both languages: it is just its two variables.
            if !path.ends_with("performance_alert/body") {
                assert_ne!(a, b, "{path} is not translated");
            }
            assert_eq!(placeholders(a), placeholders(b), "{path}");
        }
    }
    assert!(pt.pointer("/dont_show_again").is_some() && en.pointer("/dont_show_again").is_some());
}

#[test]
fn toasts_are_built_from_the_namespace_in_each_language() {
    let mut engine = engine(&[]);
    let t0 = Instant::now();
    feed(
        &mut engine,
        t0,
        0,
        &[
            ("service.crashed", service(NGINX, "nginx", "crashed")),
            ("service.running", service(NGINX, "nginx", "running")),
        ],
    );
    let notice = flush(&mut engine, t0).remove(0);
    let pt = Toast::from_notice(&notice, Language::PtBr);
    let en = Toast::from_notice(&notice, Language::EnUs);
    assert_eq!(pt.title, "Serviço reiniciado");
    assert_eq!(
        pt.body,
        "nginx/default parou inesperadamente e foi reiniciado."
    );
    assert_eq!(en.title, "Service restarted");
    assert_eq!(
        en.body,
        "nginx/default stopped unexpectedly and was restarted."
    );
    assert_eq!(pt.route, "/services");
    for kind in NoticeKind::ALL {
        assert_eq!(NoticeKind::parse(kind.id()), Some(kind));
    }
}

// --- prefs ----------------------------------------------------------------------------

#[test]
fn the_mute_list_lives_in_the_gui_prefs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(PrefsStore::load(dir.path().join("prefs.json")));
    let mutes = PrefsMutes::new(Arc::clone(&store));
    assert!(!mutes.is_muted(NoticeKind::UpdateInstalled));

    let patch = json!({ MUTED_KEY: ["update_installed", "nonsense", 3] });
    store.update(patch.as_object().unwrap()).unwrap();
    assert!(mutes.is_muted(NoticeKind::UpdateInstalled));
    assert!(!mutes.is_muted(NoticeKind::ServiceRestarted));
    assert_eq!(
        muted_from(&store.get()),
        HashSet::from([NoticeKind::UpdateInstalled])
    );

    // It survives a restart, and `null` un-mutes everything.
    let again = PrefsMutes::new(Arc::new(PrefsStore::load(dir.path().join("prefs.json"))));
    assert!(again.is_muted(NoticeKind::UpdateInstalled));
    let reset = json!({ MUTED_KEY: null });
    store.update(reset.as_object().unwrap()).unwrap();
    assert!(!mutes.is_muted(NoticeKind::UpdateInstalled));
}

// --- the tray actor -------------------------------------------------------------------

#[derive(Default)]
struct RecordingNotifier(Mutex<Vec<Toast>>);

impl RecordingNotifier {
    fn toasts(&self) -> Vec<Toast> {
        self.0.lock().unwrap().clone()
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, toast: &Toast) {
        self.0.lock().unwrap().push(toast.clone());
    }
}

/// A `Contexts` stand-in with an empty daemon whose `hello` reports `last_event_seq`.
struct Backend {
    /// What `state()` answers: the connection the tray finds when it starts.
    first: Mutex<(ConnectionStatus, u64, u64)>,
    states: broadcast::Sender<ContextStateDto>,
    sink: Mutex<Option<Arc<dyn FrameSink>>>,
}

impl Backend {
    fn new(last_event_seq: u64) -> Arc<Self> {
        Arc::new(Self {
            first: Mutex::new((ConnectionStatus::Connected, 1, last_event_seq)),
            states: broadcast::channel(16).0,
            sink: Mutex::new(None),
        })
    }

    fn connected(&self) -> ContextStateDto {
        let (status, generation, last_event_seq) = *self.first.lock().unwrap();
        Self::state_of(status, generation, last_event_seq)
    }

    /// Publishes a connection state like the `Contexts` actor does.
    fn publish(&self, status: ConnectionStatus, generation: u64, last_event_seq: u64) {
        let _ = self
            .states
            .send(Self::state_of(status, generation, last_event_seq));
    }

    fn state_of(status: ConnectionStatus, generation: u64, last_event_seq: u64) -> ContextStateDto {
        ContextStateDto {
            context: LOCAL_CONTEXT.into(),
            status,
            version: 1,
            hello: Some(HelloDto {
                session_id: SessionId::new(),
                api_version: API_VERSION,
                daemon_version: "0.2.0".into(),
                platform: "windows".into(),
                role: Role::Admin,
                transport: TransportKind::InMemory,
                local: true,
                last_event_seq,
                server_time: Timestamp::now(),
                not_implemented: Vec::new(),
            }),
            generation: Some(generation),
            attempt: 0,
            next_retry_ms: 0,
            last_error: None,
            endpoint: Some("memory:fake-root".into()),
            autostart: AutoStartDto {
                initialized: true,
                stopped_reason: None,
                next_restart_ms: None,
                restarts: 0,
            },
        }
    }

    /// One frame with these (seq, kind, payload) items.
    fn push(&self, items: &[(u64, &str, Value)]) {
        static FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let sink = self.sink.lock().unwrap().clone().expect("subscribed");
        let entries = items
            .iter()
            .map(|(seq, kind, payload)| StreamEntry::Item {
                item: json!({ "subscription_id": "01JFFFFFFFFFFFFFFFFFFFFFF1", "seq": seq,
                    "event": { "id": "01JFFFFFFFFFFFFFFFFFFFFFF2", "seq": seq,
                        "ts": "2026-01-01T00:00:00Z", "kind": kind, "payload": payload } }),
            })
            .collect();
        sink.send(StreamFrame {
            stream_id: "tray-stream".into(),
            frame: FRAME.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            entries,
        })
        .unwrap();
    }
}

impl TrayBackend for Backend {
    fn call(&self, args: CallArgs) -> BoxFuture<'static, Result<Value, ApiError>> {
        let answer = match args.method.as_str() {
            "services.status" => json!({ "instances": [], "pools": [] }),
            "sites.list" => json!({ "sites": [] }),
            "php.list" => json!({ "versions": [] }),
            _ => Value::Null,
        };
        async move { Ok(answer) }.boxed()
    }

    fn state(&self, _context: &str) -> BoxFuture<'static, Result<ContextStateDto, ApiError>> {
        let state = self.connected();
        async move { Ok(state) }.boxed()
    }

    fn watch_states(&self) -> broadcast::Receiver<ContextStateDto> {
        self.states.subscribe()
    }

    fn subscribe_events(
        &self,
        _context: &str,
        _params: Value,
        sink: Arc<dyn FrameSink>,
    ) -> BoxFuture<'static, Result<SubscribeAnswer, ApiError>> {
        *self.sink.lock().unwrap() = Some(sink);
        async {
            Ok(SubscribeAnswer {
                stream_id: "tray-stream".into(),
                method: "events.subscribe".into(),
                resume_point: Some(0),
                pending: false,
            })
        }
        .boxed()
    }

    fn ack(&self, _stream_id: &str, _frame: u64) {}

    fn unsubscribe(&self, _context: &str, _stream_id: &str) {}
}

#[derive(Default)]
struct View {
    routes: Mutex<Vec<String>>,
}

impl TrayView for View {
    fn render(&self, _render: &TrayRender) {}
    fn set_icon(&self, _icon: IconKey) {}
    fn show_window(&self) {}
    fn navigate(&self, route: &str) {
        self.routes.lock().unwrap().push(route.into());
    }
    fn open_url(&self, _url: &str) {}
    fn open_terminal(&self) {}
    fn ask_quit(&self) -> BoxFuture<'static, QuitChoice> {
        async { QuitChoice::Cancel }.boxed()
    }
    fn exit(&self) {}
}

struct Rig {
    tray: Tray,
    backend: Arc<Backend>,
    notifier: Arc<RecordingNotifier>,
    view: Arc<View>,
}

async fn rig(last_event_seq: u64, muted: &[NoticeKind]) -> Rig {
    let backend = Backend::new(last_event_seq);
    let notifier = Arc::new(RecordingNotifier::default());
    let view = Arc::new(View::default());
    let tray = Tray::spawn_with_notices(
        Arc::clone(&backend) as Arc<dyn TrayBackend>,
        Arc::clone(&view) as Arc<dyn TrayView>,
        LOCAL_CONTEXT,
        TrayOptions::default(),
        TrayAppearance::default(),
        Some(Notices {
            notifier: Arc::clone(&notifier) as Arc<dyn Notifier>,
            mutes: FakeMutes::muting(muted),
            window: TEST_WINDOW,
        }),
    );
    tray.activate();
    assert!(
        eventually(WAIT, || backend.sink.lock().unwrap().is_some()).await,
        "the tray never subscribed (the cursor is taken from `hello` before it subscribes)"
    );
    Rig {
        tray,
        backend,
        notifier,
        view,
    }
}

fn crash_and_restart(first_seq: u64) -> Vec<(u64, &'static str, Value)> {
    three_pairs()
        .into_iter()
        .enumerate()
        .map(|(i, (kind, payload))| (first_seq + i as u64, kind, payload))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tray_shows_one_grouped_toast_for_three_restarts() {
    let rig = rig(10, &[]).await;
    rig.backend.push(&crash_and_restart(11));
    assert!(eventually(WAIT, || !rig.notifier.toasts().is_empty()).await);
    tokio::time::sleep(TEST_WINDOW).await;
    let toasts = rig.notifier.toasts();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].kind, NoticeKind::ServiceRestarted);
    assert_eq!(toasts[0].route, "/services");
    // The one toast names all three services.
    for name in ["nginx/default", "mysql/default", "redis/default"] {
        assert!(toasts[0].body.contains(name), "{}", toasts[0].body);
    }
    rig.tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tray_stays_silent_for_replayed_events_and_muted_types() {
    // Everything up to seq 100 is history.
    let replayed = rig(100, &[]).await;
    replayed.backend.push(&crash_and_restart(90));
    tokio::time::sleep(TEST_WINDOW * 2).await;
    assert!(replayed.notifier.toasts().is_empty());
    // ...but the first live event after the cursor is shown.
    replayed
        .backend
        .push(&[(101, "tool.updated", updated("php", "8.3.37-nts-vs17-x64"))]);
    assert!(eventually(WAIT, || replayed.notifier.toasts().len() == 1).await);
    replayed.tray.stop().await;

    let muted = rig(10, &[NoticeKind::ServiceRestarted]).await;
    muted.backend.push(&crash_and_restart(11));
    tokio::time::sleep(TEST_WINDOW * 2).await;
    assert!(muted.notifier.toasts().is_empty());
    muted.tray.stop().await;
}

/// The real order on a reconnection: the connection is lost, the client resubscribes with
/// `resume_from` and the daemon replays what was missed, and only then does the new
/// connection's state (with its `hello`) reach the tray.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_that_beats_the_new_connection_state_stays_silent() {
    let rig = rig(10, &[]).await;
    rig.backend.publish(ConnectionStatus::Reconnecting, 1, 10);
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Services crashed and restarted while the GUI was away: seq 11..16 are replayed.
    rig.backend.push(&crash_and_restart(11));
    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.backend.publish(ConnectionStatus::Connected, 2, 16);
    tokio::time::sleep(TEST_WINDOW * 2).await;
    assert!(
        rig.notifier.toasts().is_empty(),
        "{:?}",
        rig.notifier.toasts()
    );

    // A live event after the new cursor is shown.
    rig.backend
        .push(&[(17, "tool.updated", updated("php", "8.3.37-nts-vs17-x64"))]);
    assert!(eventually(WAIT, || rig.notifier.toasts().len() == 1).await);
    assert_eq!(rig.notifier.toasts()[0].kind, NoticeKind::UpdateInstalled);
    rig.tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clicking_a_toast_opens_the_related_screen() {
    let rig = rig(0, &[]).await;
    rig.tray.notification_clicked(NoticeKind::InstallCompleted);
    assert!(eventually(WAIT, || !rig.view.routes.lock().unwrap().is_empty()).await);
    assert_eq!(*rig.view.routes.lock().unwrap(), ["/catalog"]);
    rig.tray.stop().await;
}
