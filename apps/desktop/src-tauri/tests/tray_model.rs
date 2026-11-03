//! The tray (M2-E1-T15, docs/07 §7): the pure model reducer, the actor over a fake
//! `Contexts`, the quit flow, and the actor over a real forge-ipc test server.

mod common;

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use common::*;
use forge_ipc::API_VERSION;
use forge_ipc::ApiError;
use forge_ipc::api::methods::{php_list, services_status, sites_list};
use forge_ipc::api::{
    HelloDto, NoParams, PhpVersionListDto, Role, ServiceStatusDto, SiteListDto, TransportKind,
};
use forge_ipc::base::Timestamp;
use forge_ipc::base::event::EventKind;
use forge_ipc::base::ids::{RequestId, SessionId};
use forge_ipc::server::Router;
use forgebox_desktop::bridge::{
    AutoStartDto, BridgeOptions, CallArgs, ConnectionStatus, ContextStateDto, FrameSink,
    LOCAL_CONTEXT, StreamEntry, StreamFrame, SubscribeAnswer,
};
use forgebox_desktop::i18n::Language;
use forgebox_desktop::tray::icons::IconKey;
use forgebox_desktop::tray::model::{
    Effect, Link, MenuNode, Scope, Snapshot, TrayAction, TrayModel, TrayState,
};
use forgebox_desktop::tray::{
    Click, QuitChoice, QuitOutcome, Tray, TrayAppearance, TrayBackend, TrayOptions, TrayRender,
    TrayView, quit_flow,
};

// --- fixtures -------------------------------------------------------------------------

const NGINX: &str = "01JAAAAAAAAAAAAAAAAAAAAAA1";
const MYSQL: &str = "01JAAAAAAAAAAAAAAAAAAAAAA2";
const REDIS: &str = "01JAAAAAAAAAAAAAAAAAAAAAA3";
const PHP83: &str = "01JBBBBBBBBBBBBBBBBBBBBBB1";
const PHP84: &str = "01JBBBBBBBBBBBBBBBBBBBBBB2";
const VERSION: &str = "01JCCCCCCCCCCCCCCCCCCCCCC1";

fn instance(id: &str, tool: &str, version: &str, state: &str, autostart: bool) -> Value {
    json!({
        "id": id, "tool": tool, "version_id": VERSION, "version": version, "name": "default",
        "state": state, "pid": null, "ports": [], "autostart": autostart, "started_at": null,
        "cpu_pct": null, "rss_bytes": null, "restarts": 0, "last_error": null
    })
}

fn status(instances: Vec<Value>) -> Value {
    json!({ "instances": instances, "pools": [] })
}

fn site(host: &str) -> Value {
    json!({
        "id": "01JDDDDDDDDDDDDDDDDDDDDDD1", "slug": host.replace('.', "-"), "host": host,
        "aliases": [], "kind": "php", "root": "C:/www/loja", "driver": null, "server": "nginx",
        "php": null, "upstream": null, "websocket": false, "secure": true,
        "http_redirect": true, "env": {}, "xdebug": "off", "parked": false, "enabled": true,
        "case_sensitive": true, "url": format!("https://{host}"), "processes": [],
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
    })
}

fn php_version(id: &str, version: &str, current: bool) -> Value {
    json!({
        "version_id": id, "version": version, "series": &version[..3], "thread_safety": "nts",
        "compiler": "vs17", "arch": "x64", "is_current_cli": current, "pool": null,
        "sites": 0, "xdebug": "off"
    })
}

fn sites(list: Vec<Value>) -> Value {
    json!({ "sites": list })
}

fn php(list: Vec<Value>) -> Value {
    json!({ "versions": list })
}

fn hello(not_implemented: &[&str], local: bool) -> HelloDto {
    HelloDto {
        session_id: SessionId::new(),
        api_version: API_VERSION,
        daemon_version: "0.2.0".into(),
        platform: "windows".into(),
        role: Role::Admin,
        transport: TransportKind::InMemory,
        local,
        last_event_seq: 0,
        server_time: Timestamp::now(),
        not_implemented: not_implemented.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn state(
    context: &str,
    status: ConnectionStatus,
    hello: Option<HelloDto>,
    generation: u64,
) -> ContextStateDto {
    ContextStateDto {
        context: context.into(),
        status,
        version: generation,
        hello,
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

/// Everything implemented except what is listed.
fn connected(not_implemented: &[&str]) -> ContextStateDto {
    state(
        LOCAL_CONTEXT,
        ConnectionStatus::Connected,
        Some(hello(not_implemented, true)),
        1,
    )
}

fn service_event(id: &str, tool: &str, state: &str) -> Value {
    json!({
        "v": 1, "instance_id": id, "name": format!("{tool}/default"), "tool": tool,
        "state": state, "pid": null, "strategy": null, "exit_code": null, "error": null
    })
}

fn snapshot_of(services: Value, sites_v: Value, php_v: Value) -> Snapshot {
    Snapshot {
        services: Some(Snapshot::services_from(
            &serde_json::from_value::<ServiceStatusDto>(services).unwrap(),
        )),
        sites: Some(Snapshot::sites_from(
            &serde_json::from_value::<SiteListDto>(sites_v)
                .unwrap()
                .sites,
        )),
        php: Some(Snapshot::php_from(
            &serde_json::from_value::<PhpVersionListDto>(php_v)
                .unwrap()
                .versions,
        )),
        mail_unread: None,
    }
}

/// nginx and mysql (autostart, stopped), redis (manual, stopped); PHP 8.3 current, 8.4.
fn loaded_model() -> TrayModel {
    let mut model = TrayModel::new(LOCAL_CONTEXT);
    assert_eq!(
        model.set_connection(&connected(&["mail.status", "terminal.open"])),
        Effect::Resync(Scope::ALL)
    );
    model.apply_snapshot(snapshot_of(
        status(vec![
            instance(NGINX, "nginx", "1.27.4", "stopped", true),
            instance(MYSQL, "mysql", "8.4.3", "stopped", true),
            instance(REDIS, "redis", "7.4.1", "stopped", false),
        ]),
        sites(vec![site("loja.test")]),
        php(vec![
            php_version(PHP83, "8.3.36", true),
            php_version(PHP84, "8.4.10", false),
        ]),
    ));
    model
}

fn ev(model: &mut TrayModel, kind: &str, payload: Value) -> Effect {
    model.apply_event(kind, &payload)
}

fn checked(model: &TrayModel, id: &str) -> Option<bool> {
    model
        .menu(Language::PtBr)
        .find(id)
        .and_then(MenuNode::checked)
}

fn entry_enabled(model: &TrayModel, id: &str) -> Option<bool> {
    model.menu(Language::PtBr).find(id).map(MenuNode::enabled)
}

// --- reducer --------------------------------------------------------------------------

#[test]
fn precedence_is_error_warning_starting_updating_running_stopped() {
    use TrayState::*;
    assert_eq!(
        TrayState::PRECEDENCE,
        [Error, Warning, Starting, Updating, Running, Stopped]
    );
    let all = [Stopped, Running, Updating, Starting, Warning, Error];
    for (i, expected) in TrayState::PRECEDENCE.iter().enumerate() {
        // Every state below `expected` holds too: `expected` still wins.
        let holding: Vec<TrayState> = TrayState::PRECEDENCE[i..].to_vec();
        assert_eq!(TrayState::highest(&holding), *expected);
    }
    assert_eq!(TrayState::highest(&all), Error);
    assert_eq!(TrayState::highest(&[Running, Updating]), Updating);
    assert_eq!(TrayState::highest(&[Starting, Updating, Running]), Starting);
    assert_eq!(TrayState::highest(&[]), Stopped);
}

#[test]
fn base_until_connected_and_loaded() {
    let mut model = TrayModel::new(LOCAL_CONTEXT);
    assert_eq!(model.state(), TrayState::Base);
    assert_eq!(model.link, Link::Pending);
    // Opening / first connection attempts are not errors.
    for status in [
        ConnectionStatus::Idle,
        ConnectionStatus::Connecting,
        ConnectionStatus::Starting,
        ConnectionStatus::Uninitialized,
    ] {
        model.set_connection(&state(LOCAL_CONTEXT, status, None, 0));
        assert_eq!(model.state(), TrayState::Base, "{status:?}");
    }
    model.set_connection(&connected(&[]));
    assert_eq!(model.state(), TrayState::Base, "no snapshot yet");
    assert_eq!(model.status_line(Language::PtBr), "Conectando ao daemon…");
    model.apply_snapshot(Snapshot {
        services: Some(Vec::new()),
        ..Snapshot::default()
    });
    assert_eq!(model.state(), TrayState::Stopped);
    assert_eq!(model.status_line(Language::PtBr), "Nenhum serviço criado");
}

#[test]
fn service_event_sequences_map_to_the_aggregate_state() {
    let mut m = loaded_model();
    assert_eq!(m.state(), TrayState::Stopped, "nothing runs");
    assert_eq!(m.status_line(Language::PtBr), "0 de 3 serviços em execução");

    assert_eq!(
        ev(
            &mut m,
            "service.starting",
            service_event(NGINX, "nginx", "starting")
        ),
        Effect::Changed
    );
    assert_eq!(m.state(), TrayState::Starting);
    ev(
        &mut m,
        "service.running",
        service_event(NGINX, "nginx", "running"),
    );
    assert_eq!(m.state(), TrayState::Running);
    assert_eq!(m.status_line(Language::PtBr), "1 de 3 serviços em execução");
    // A repeated state changes nothing.
    assert_eq!(
        ev(
            &mut m,
            "service.running",
            service_event(NGINX, "nginx", "running")
        ),
        Effect::None
    );

    ev(
        &mut m,
        "service.starting",
        service_event(MYSQL, "mysql", "starting"),
    );
    assert_eq!(m.state(), TrayState::Starting, "starting > running");
    ev(
        &mut m,
        "service.running",
        service_event(MYSQL, "mysql", "running"),
    );
    ev(
        &mut m,
        "service.running",
        service_event(REDIS, "redis", "running"),
    );
    assert_eq!(m.state(), TrayState::Running);
    assert_eq!(
        m.status_line(Language::PtBr),
        "Todos os serviços em execução"
    );
    assert_eq!(m.status_line(Language::EnUs), "All services running");

    // Updates pending: updating > running; cleared by the next (empty) list.
    ev(
        &mut m,
        "tool.updates_available",
        json!({ "v": 1, "updates": [{ "tool": "php", "series": "8.3", "from": "8.3.36", "to": "8.3.37" }], "count": 1 }),
    );
    assert_eq!(m.state(), TrayState::Updating);
    ev(
        &mut m,
        "tool.updates_available",
        json!({ "v": 1, "updates": [], "count": 0 }),
    );
    assert_eq!(m.state(), TrayState::Running);

    // One enabled service crashes while others run: warning.
    ev(
        &mut m,
        "service.crashed",
        service_event(MYSQL, "mysql", "crashed"),
    );
    assert_eq!(m.state(), TrayState::Warning);
    assert_eq!(m.status_line(Language::PtBr), "1 com falha");
    // Warning beats starting.
    ev(
        &mut m,
        "service.starting",
        service_event(REDIS, "redis", "starting"),
    );
    assert_eq!(m.state(), TrayState::Warning);
    // Every enabled service failed: error (redis is not enabled).
    ev(
        &mut m,
        "service.failed",
        service_event(NGINX, "nginx", "failed"),
    );
    assert_eq!(m.state(), TrayState::Error);
    assert_eq!(m.status_line(Language::PtBr), "2 com falha");
    ev(
        &mut m,
        "service.running",
        service_event(MYSQL, "mysql", "running"),
    );
    assert_eq!(m.state(), TrayState::Warning);

    // Connection loss: error, whatever the services were.
    let effect = m.set_connection(&state(
        LOCAL_CONTEXT,
        ConnectionStatus::Reconnecting,
        None,
        1,
    ));
    assert_eq!(effect, Effect::Changed);
    assert_eq!(m.link, Link::Down);
    assert_eq!(m.state(), TrayState::Error);
    assert_eq!(m.status_line(Language::PtBr), "Daemon indisponível");
    // A deliberate stop after a connection is down too; coming back resyncs.
    m.set_connection(&state(LOCAL_CONTEXT, ConnectionStatus::Stopped, None, 1));
    assert_eq!(m.state(), TrayState::Error);
    assert_eq!(
        m.set_connection(&connected(&[])),
        Effect::Resync(Scope::ALL)
    );
    assert_eq!(m.state(), TrayState::Warning, "model kept until the resync");
    // Same connection again: nothing to fetch.
    assert_eq!(m.set_connection(&connected(&[])), Effect::None);
    // A new connection (generation 2) without a loss in between still resyncs.
    assert_eq!(
        m.set_connection(&state(
            LOCAL_CONTEXT,
            ConnectionStatus::Connected,
            Some(hello(&[], true)),
            2
        )),
        Effect::Resync(Scope::ALL)
    );
}

#[test]
fn install_and_update_jobs_and_maintenance_mean_updating() {
    let mut m = loaded_model();
    ev(
        &mut m,
        "service.running",
        service_event(NGINX, "nginx", "running"),
    );
    assert_eq!(m.state(), TrayState::Running);
    let job = "01JEEEEEEEEEEEEEEEEEEEEEE1";
    ev(
        &mut m,
        "job.started",
        json!({ "v": 1, "job_id": job, "name": "tools.install" }),
    );
    assert_eq!(m.state(), TrayState::Updating);
    // Other jobs do not count.
    let mut other = loaded_model();
    ev(
        &mut other,
        "job.started",
        json!({ "v": 1, "job_id": job, "name": "sites.add" }),
    );
    assert_eq!(other.state(), TrayState::Stopped);
    ev(
        &mut m,
        "job.done",
        json!({ "v": 1, "job_id": job, "name": "tools.install", "duration_ms": 10, "result": null }),
    );
    assert_eq!(m.state(), TrayState::Running);
    ev(
        &mut m,
        "service.maintenance",
        service_event(MYSQL, "mysql", "maintenance"),
    );
    assert_eq!(m.state(), TrayState::Updating);
    // The app updater's announcement counts too.
    let mut app = loaded_model();
    app.app_update = true;
    assert_eq!(app.state(), TrayState::Updating);
}

#[test]
fn service_toggles_follow_the_instances() {
    let mut m = loaded_model();
    let start = |id: &str| {
        TrayAction::ToggleService {
            id: id.into(),
            start: true,
        }
        .id()
    };
    let stop = |id: &str| {
        TrayAction::ToggleService {
            id: id.into(),
            start: false,
        }
        .id()
    };
    assert_eq!(checked(&m, &start(NGINX)), Some(false));
    assert_eq!(entry_enabled(&m, &start(NGINX)), Some(true));
    ev(
        &mut m,
        "service.starting",
        service_event(NGINX, "nginx", "starting"),
    );
    assert_eq!(entry_enabled(&m, &start(NGINX)), Some(false), "busy");
    ev(
        &mut m,
        "service.running",
        service_event(NGINX, "nginx", "running"),
    );
    assert_eq!(checked(&m, &stop(NGINX)), Some(true));
    assert!(m.menu(Language::PtBr).find(&start(NGINX)).is_none());
    let menu = m.menu(Language::PtBr);
    let labels: Vec<&str> = menu
        .find("services")
        .unwrap()
        .children()
        .iter()
        .filter_map(MenuNode::label)
        .collect();
    assert_eq!(
        labels,
        [
            "mysql 8.4.3",
            "nginx 1.27.4",
            "redis 7.4.1",
            "Abrir Serviços"
        ]
    );
    // An unknown instance asks for services.status again.
    assert_eq!(
        ev(
            &mut m,
            "service.running",
            service_event("01JAAAAAAAAAAAAAAAAAAAAAA9", "caddy", "running")
        ),
        Effect::Resync(Scope::SERVICES)
    );
    // Sites change: sites.list again.
    assert_eq!(
        ev(&mut m, "site.created", json!({ "v": 1 })),
        Effect::Resync(Scope::SITES)
    );
}

#[test]
fn php_radio_follows_tool_current_changed() {
    let mut m = loaded_model();
    let id83 = TrayAction::UsePhp("8.3.36".into()).id();
    let id84 = TrayAction::UsePhp("8.4.10".into()).id();
    assert_eq!(checked(&m, &id83), Some(true));
    assert_eq!(checked(&m, &id84), Some(false));
    assert_eq!(entry_enabled(&m, &id83), Some(false), "already current");
    // By version_id.
    let effect = ev(
        &mut m,
        "tool.current_changed",
        json!({ "v": 1, "tool": "php", "from": "8.3.36-nts-vs17-x64", "to": "8.4.10-nts-vs17-x64", "version_id": PHP84, "elapsed_us": 900 }),
    );
    assert_eq!(effect, Effect::Changed);
    assert_eq!(checked(&m, &id83), Some(false));
    assert_eq!(checked(&m, &id84), Some(true));
    assert!(m.tooltip(Language::PtBr).contains("PHP CLI 8.4.10"));
    // By folder name when the version id is missing.
    ev(
        &mut m,
        "tool.current_changed",
        json!({ "v": 1, "tool": "php", "from": null, "to": "8.3.36-nts-vs17-x64", "version_id": null, "elapsed_us": 900 }),
    );
    assert_eq!(checked(&m, &id83), Some(true));
    // Another tool does not touch the PHP radio.
    assert_eq!(
        ev(
            &mut m,
            "tool.current_changed",
            json!({ "v": 1, "tool": "node", "from": null, "to": "22.1.0", "version_id": null, "elapsed_us": 1 }),
        ),
        Effect::None
    );
    // A version the tray does not know: php.list again.
    assert_eq!(
        ev(
            &mut m,
            "tool.current_changed",
            json!({ "v": 1, "tool": "php", "from": null, "to": "8.5.0-nts-vs17-x64", "version_id": null, "elapsed_us": 1 }),
        ),
        Effect::Resync(Scope::PHP)
    );
}

#[test]
fn mail_and_terminal_stay_hidden_while_not_implemented() {
    let m = loaded_model();
    let menu = m.menu(Language::PtBr);
    assert!(menu.find("mail").is_none());
    assert!(menu.find("terminal").is_none());
    assert!(!m.terminal_visible());

    let mut m = TrayModel::new(LOCAL_CONTEXT);
    m.set_connection(&connected(&[]));
    m.apply_snapshot(Snapshot {
        services: Some(Vec::new()),
        mail_unread: Some(3),
        ..Snapshot::default()
    });
    let menu = m.menu(Language::PtBr);
    assert_eq!(
        menu.find("mail").and_then(MenuNode::label),
        Some("Mail (3 não lidos)")
    );
    assert!(menu.find("terminal").is_some());
    ev(
        &mut m,
        "mail.changed",
        json!({ "v": 1, "instance_id": MYSQL, "messages": 4, "unread": 0, "new_messages": 0 }),
    );
    assert_eq!(
        m.menu(Language::EnUs)
            .find("mail")
            .and_then(MenuNode::label),
        Some("Mail")
    );
    // Not connected: unknown, so hidden.
    m.set_connection(&state(
        LOCAL_CONTEXT,
        ConnectionStatus::Reconnecting,
        None,
        1,
    ));
    assert!(m.menu(Language::PtBr).find("mail").is_none());
}

#[test]
fn start_restart_and_stop_all_follow_the_state() {
    let mut m = loaded_model();
    let ids = |m: &TrayModel| -> Vec<String> {
        m.menu(Language::PtBr)
            .top_level()
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    // Everything stopped: start shown, restart and stop disabled.
    assert!(ids(&m).iter().any(|id| id == "start_all"));
    assert_eq!(entry_enabled(&m, "restart_all"), Some(false));
    assert_eq!(entry_enabled(&m, "stop_all"), Some(false));
    // Something starting: stop applies.
    ev(
        &mut m,
        "service.starting",
        service_event(NGINX, "nginx", "starting"),
    );
    assert_eq!(entry_enabled(&m, "stop_all"), Some(true));
    assert_eq!(entry_enabled(&m, "restart_all"), Some(false));
    ev(
        &mut m,
        "service.running",
        service_event(NGINX, "nginx", "running"),
    );
    assert!(
        ids(&m).iter().any(|id| id == "start_all"),
        "mysql is still stopped"
    );
    assert_eq!(entry_enabled(&m, "restart_all"), Some(true));
    // Every enabled service runs: start hidden (redis is manual).
    ev(
        &mut m,
        "service.running",
        service_event(MYSQL, "mysql", "running"),
    );
    assert!(!ids(&m).iter().any(|id| id == "start_all"));
    // A failed enabled service can be started again.
    ev(
        &mut m,
        "service.failed",
        service_event(MYSQL, "mysql", "failed"),
    );
    assert!(ids(&m).iter().any(|id| id == "start_all"));
    // Disconnected: nothing to act on.
    m.set_connection(&state(LOCAL_CONTEXT, ConnectionStatus::Stopped, None, 1));
    assert!(!ids(&m).iter().any(|id| id == "start_all"));
    assert_eq!(entry_enabled(&m, "restart_all"), Some(false));
    assert_eq!(entry_enabled(&m, "stop_all"), Some(false));
    assert_eq!(entry_enabled(&m, "sites"), Some(false));
    assert_eq!(entry_enabled(&m, "open"), Some(true));
}

#[test]
fn the_menu_has_the_reference_structure() {
    let m = loaded_model();
    let menu = m.menu(Language::PtBr);
    assert_eq!(
        menu.top_level(),
        [
            "header",
            "status",
            "open",
            "services",
            "sites",
            "php",
            "-",
            "start_all",
            "restart_all",
            "stop_all",
            "-",
            "logs",
            "-",
            "settings",
            "quit"
        ]
    );
    let header = menu.find("header").unwrap();
    assert_eq!(header.label(), Some("Forgebox · Local"));
    let contexts: Vec<(&str, bool)> = header
        .children()
        .iter()
        .map(|n| (n.label().unwrap(), n.enabled()))
        .collect();
    assert_eq!(
        contexts,
        [("● Local", false), ("Adicionar remoto (M6)", false)]
    );
    assert_eq!(menu.find("status").map(MenuNode::enabled), Some(false));
    let site = TrayAction::OpenSite("https://loja.test".into()).id();
    assert_eq!(
        menu.find(&site).and_then(MenuNode::label),
        Some("loja.test")
    );
    assert_eq!(
        m.menu(Language::EnUs)
            .find("open")
            .and_then(MenuNode::label),
        Some("Open dashboard")
    );
    // Tooltip: title, status, PHP CLI.
    assert_eq!(
        m.tooltip(Language::PtBr),
        "Forgebox · Local\n0 de 3 serviços em execução\nPHP CLI 8.3.36"
    );
}

#[test]
fn a_remote_connection_marks_the_icon_and_the_tooltip() {
    let mut m = TrayModel::new("hml");
    assert!(m.remote);
    let mut local = TrayModel::new(LOCAL_CONTEXT);
    local.set_connection(&state(
        LOCAL_CONTEXT,
        ConnectionStatus::Connected,
        Some(hello(&[], false)),
        1,
    ));
    assert!(local.remote, "hello.local = false");
    m.set_connection(&state(
        "hml",
        ConnectionStatus::Connected,
        Some(hello(&[], false)),
        1,
    ));
    assert!(m.tooltip(Language::PtBr).contains("Remoto: hml"));
    assert_eq!(
        m.menu(Language::PtBr)
            .find("header")
            .and_then(MenuNode::label),
        Some("Forgebox · hml")
    );
    // Another context's state is not this model's.
    assert_eq!(
        m.set_connection(&state(LOCAL_CONTEXT, ConnectionStatus::Stopped, None, 1)),
        Effect::None
    );
}

#[test]
fn actions_round_trip_through_menu_ids() {
    let actions = [
        TrayAction::OpenWindow,
        TrayAction::SwitchContext("hml".into()),
        TrayAction::ToggleService {
            id: NGINX.into(),
            start: true,
        },
        TrayAction::ToggleService {
            id: NGINX.into(),
            start: false,
        },
        TrayAction::OpenServices,
        TrayAction::OpenSite("https://loja.test:8443/x".into()),
        TrayAction::UsePhp("8.3.36".into()),
        TrayAction::StartAll,
        TrayAction::RestartAll,
        TrayAction::StopAll,
        TrayAction::Logs,
        TrayAction::Mail,
        TrayAction::Terminal,
        TrayAction::Settings,
        TrayAction::Quit,
    ];
    for action in actions {
        assert_eq!(TrayAction::parse(&action.id()), Some(action.clone()));
    }
    for junk in [
        "",
        "status",
        "service:pause:x",
        "service:start:",
        "site:",
        "nope:x",
    ] {
        assert_eq!(TrayAction::parse(junk), None, "{junk}");
    }
    assert_eq!(
        TrayAction::UsePhp("8.4".into()).api_call(),
        Some(("tools.use", json!({ "tool": "php", "version": "8.4" })))
    );
    assert_eq!(TrayAction::Logs.route(), Some("/logs"));
    assert_eq!(TrayAction::Settings.route(), Some("/settings"));
}

// --- fakes ----------------------------------------------------------------------------

/// A fake `Contexts`: answers the snapshot calls, records every call, and hands the test
/// the sink of the tray's stream.
struct FakeBackend {
    context: String,
    states: broadcast::Sender<ContextStateDto>,
    calls: Mutex<Vec<CallArgs>>,
    sink: Mutex<Option<Arc<dyn FrameSink>>>,
    answers: Mutex<Vec<(String, Value)>>,
    /// `daemon.stop` waits this long (never answers when `None`).
    stop_delay: Option<Duration>,
    not_implemented: Vec<&'static str>,
}

impl FakeBackend {
    fn new(context: &str) -> Arc<Self> {
        Self::with(
            context,
            Some(Duration::from_millis(20)),
            vec!["mail.status"],
        )
    }

    fn with(
        context: &str,
        stop_delay: Option<Duration>,
        not_implemented: Vec<&'static str>,
    ) -> Arc<Self> {
        Arc::new(Self {
            context: context.into(),
            states: broadcast::channel(16).0,
            calls: Mutex::new(Vec::new()),
            sink: Mutex::new(None),
            answers: Mutex::new(vec![
                (
                    "services.status".into(),
                    status(vec![
                        instance(NGINX, "nginx", "1.27.4", "stopped", true),
                        instance(MYSQL, "mysql", "8.4.3", "running", true),
                    ]),
                ),
                ("sites.list".into(), sites(vec![site("loja.test")])),
                (
                    "php.list".into(),
                    php(vec![php_version(PHP83, "8.3.36", true)]),
                ),
            ]),
            stop_delay,
            not_implemented,
        })
    }

    fn calls(&self) -> Vec<CallArgs> {
        self.calls.lock().unwrap().clone()
    }

    fn count(&self, method: &str) -> usize {
        self.calls().iter().filter(|c| c.method == method).count()
    }

    fn push(&self, entries: Vec<StreamEntry>) {
        let sink = self.sink.lock().unwrap().clone().expect("subscribed");
        static FRAME: AtomicUsize = AtomicUsize::new(0);
        sink.send(StreamFrame {
            stream_id: "tray-stream".into(),
            frame: FRAME.fetch_add(1, Ordering::SeqCst) as u64 + 1,
            entries,
        })
        .unwrap();
    }
}

fn event_entry(kind: &str, payload: Value) -> StreamEntry {
    StreamEntry::Item {
        item: json!({ "subscription_id": "01JFFFFFFFFFFFFFFFFFFFFFF1", "seq": 1,
            "event": { "id": "01JFFFFFFFFFFFFFFFFFFFFFF2", "seq": 1, "ts": "2026-01-01T00:00:00Z",
                "kind": kind, "payload": payload } }),
    }
}

impl TrayBackend for FakeBackend {
    fn call(&self, args: CallArgs) -> BoxFuture<'static, Result<Value, ApiError>> {
        self.calls.lock().unwrap().push(args.clone());
        let answer = self
            .answers
            .lock()
            .unwrap()
            .iter()
            .find(|(m, _)| *m == args.method)
            .map(|(_, v)| v.clone());
        let delay = self.stop_delay;
        async move {
            match (args.method.as_str(), answer) {
                ("daemon.stop", _) => match delay {
                    Some(delay) => {
                        tokio::time::sleep(delay).await;
                        Ok(json!({ "ok": true }))
                    }
                    None => futures::future::pending().await,
                },
                (_, Some(value)) => Ok(value),
                ("mail.status", None) => Err(ApiError::new("not_implemented", "no mail")),
                _ => Ok(json!({ "id": RequestId::new().to_string(), "name": args.method })),
            }
        }
        .boxed()
    }

    fn state(&self, context: &str) -> BoxFuture<'static, Result<ContextStateDto, ApiError>> {
        let answer = state(
            context,
            ConnectionStatus::Connected,
            Some(hello(&self.not_implemented, context == LOCAL_CONTEXT)),
            1,
        );
        let known = context == self.context;
        async move {
            if known {
                Ok(answer)
            } else {
                Err(ApiError::new("not_found", "unknown context"))
            }
        }
        .boxed()
    }

    fn watch_states(&self) -> broadcast::Receiver<ContextStateDto> {
        self.states.subscribe()
    }

    fn subscribe_events(
        &self,
        context: &str,
        params: Value,
        sink: Arc<dyn FrameSink>,
    ) -> BoxFuture<'static, Result<SubscribeAnswer, ApiError>> {
        assert_eq!(context, self.context, "the tray subscribes on its context");
        assert!(
            params["filter"]["kinds"]
                .as_array()
                .is_some_and(|k| !k.is_empty())
        );
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
struct Recorded {
    renders: Vec<(Instant, TrayRender)>,
    icons: Vec<IconKey>,
    shown: usize,
    routes: Vec<String>,
    urls: Vec<String>,
    terminals: usize,
    exits: usize,
}

/// Records what the tray asks of the native side.
struct RecordingView {
    rec: Mutex<Recorded>,
    quit: Mutex<QuitChoice>,
}

impl RecordingView {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rec: Mutex::new(Recorded::default()),
            quit: Mutex::new(QuitChoice::Cancel),
        })
    }

    fn last(&self) -> Option<TrayRender> {
        self.rec
            .lock()
            .unwrap()
            .renders
            .last()
            .map(|(_, r)| r.clone())
    }

    fn renders(&self) -> usize {
        self.rec.lock().unwrap().renders.len()
    }

    /// When the first render matching `check` landed.
    fn rendered_at(&self, check: impl Fn(&TrayRender) -> bool) -> Option<Instant> {
        self.rec
            .lock()
            .unwrap()
            .renders
            .iter()
            .find(|(_, r)| check(r))
            .map(|(at, _)| *at)
    }
}

impl TrayView for RecordingView {
    fn render(&self, render: &TrayRender) {
        self.rec
            .lock()
            .unwrap()
            .renders
            .push((Instant::now(), render.clone()));
    }
    fn set_icon(&self, icon: IconKey) {
        self.rec.lock().unwrap().icons.push(icon);
    }
    fn show_window(&self) {
        self.rec.lock().unwrap().shown += 1;
    }
    fn navigate(&self, route: &str) {
        self.rec.lock().unwrap().routes.push(route.into());
    }
    fn open_url(&self, url: &str) {
        self.rec.lock().unwrap().urls.push(url.into());
    }
    fn open_terminal(&self) {
        self.rec.lock().unwrap().terminals += 1;
    }
    fn ask_quit(&self) -> BoxFuture<'static, QuitChoice> {
        let choice = *self.quit.lock().unwrap();
        async move { choice }.boxed()
    }
    fn exit(&self) {
        self.rec.lock().unwrap().exits += 1;
    }
}

fn fast_options() -> TrayOptions {
    TrayOptions {
        quit_timeout: Duration::from_millis(300),
        frame_interval: Duration::from_millis(20),
        ..TrayOptions::default()
    }
}

async fn loaded_tray(backend: &Arc<FakeBackend>, view: &Arc<RecordingView>) -> Tray {
    loaded_tray_with(backend, view, TrayAppearance::default()).await
}

async fn loaded_tray_with(
    backend: &Arc<FakeBackend>,
    view: &Arc<RecordingView>,
    appearance: TrayAppearance,
) -> Tray {
    let tray = Tray::spawn(
        Arc::clone(backend) as Arc<dyn TrayBackend>,
        Arc::clone(view) as Arc<dyn TrayView>,
        &backend.context,
        fast_options(),
        appearance,
    );
    tray.activate();
    assert!(
        eventually(WAIT, || view.last().is_some_and(|r| r.icon.state
            == TrayState::Warning
            || r.icon.state == TrayState::Running))
        .await,
        "{:?}",
        view.last()
    );
    tray
}

// --- actor over a fake Contexts -------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_connects_before_activation() {
    let backend = FakeBackend::new(LOCAL_CONTEXT);
    let view = RecordingView::new();
    let tray = Tray::spawn(
        Arc::clone(&backend) as Arc<dyn TrayBackend>,
        Arc::clone(&view) as Arc<dyn TrayView>,
        LOCAL_CONTEXT,
        fast_options(),
        TrayAppearance::default(),
    );
    assert!(eventually(WAIT, || view.renders() == 1).await);
    assert_eq!(view.last().unwrap().icon.state, TrayState::Base);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(backend.calls().is_empty(), "{:?}", backend.calls());
    assert!(backend.sink.lock().unwrap().is_none());
    tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actions_use_a_fresh_request_id_and_the_active_context() {
    for context in [LOCAL_CONTEXT, "hml"] {
        let backend = FakeBackend::new(context);
        let view = RecordingView::new();
        let tray = loaded_tray(&backend, &view).await;
        let before = backend.calls().len();
        for action in [
            TrayAction::ToggleService {
                id: NGINX.into(),
                start: true,
            },
            TrayAction::ToggleService {
                id: MYSQL.into(),
                start: false,
            },
            TrayAction::UsePhp("8.3.36".into()),
            TrayAction::StartAll,
            TrayAction::RestartAll,
            TrayAction::StopAll,
            TrayAction::StopAll,
        ] {
            tray.menu_event(&action.id());
        }
        assert!(eventually(WAIT, || backend.calls().len() == before + 7).await);
        let actions = &backend.calls()[before..];
        let methods: Vec<&str> = actions.iter().map(|c| c.method.as_str()).collect();
        assert_eq!(
            methods,
            [
                "services.start",
                "services.stop",
                "tools.use",
                "services.start",
                "services.restart",
                "services.stop",
                "services.stop"
            ]
        );
        assert_eq!(actions[0].params, Some(json!({ "targets": [NGINX] })));
        assert_eq!(actions[3].params, Some(json!({ "targets": [] })));
        let mut ids = BTreeSet::new();
        for call in actions {
            assert_eq!(call.context, context, "{call:?}");
            let id = call.request_id.as_deref().expect("request_id set");
            RequestId::from_str(id).expect("a ULID");
            assert!(ids.insert(id.to_owned()), "fresh per action: {id}");
        }
        assert!(actions.iter().all(|c| c.timeout_ms.is_some()));
        tray.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_side_actions_never_reach_the_daemon() {
    let backend = FakeBackend::with(LOCAL_CONTEXT, None, vec![]);
    let view = RecordingView::new();
    let tray = loaded_tray(&backend, &view).await;
    let before = backend.calls().len();
    for action in [
        TrayAction::OpenWindow,
        TrayAction::OpenServices,
        TrayAction::Logs,
        TrayAction::Settings,
        TrayAction::Mail,
        TrayAction::OpenSite("https://loja.test".into()),
        TrayAction::Terminal,
    ] {
        tray.menu_event(&action.id());
    }
    tray.click(Click::Left);
    tray.click(Click::Middle);
    assert!(
        eventually(WAIT, || {
            let rec = view.rec.lock().unwrap();
            rec.terminals == 2 && rec.shown == 2 && rec.urls.len() == 1 && rec.routes.len() == 4
        })
        .await
    );
    {
        let rec = view.rec.lock().unwrap();
        assert_eq!(rec.routes, ["/services", "/logs", "/settings", "/mail"]);
        assert_eq!(rec.urls, ["https://loja.test"]);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        backend.calls().len(),
        before,
        "{:?}",
        &backend.calls()[before..]
    );
    tray.stop().await;

    // Terminal not implemented: neither the entry nor the middle click opens one.
    let backend = FakeBackend::with(LOCAL_CONTEXT, None, vec!["terminal.open"]);
    let view = RecordingView::new();
    let tray = loaded_tray(&backend, &view).await;
    assert!(view.last().unwrap().menu.find("terminal").is_none());
    tray.menu_event("terminal");
    tray.click(Click::Middle);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(view.rec.lock().unwrap().terminals, 0);
    tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_rebuild_debounced_and_resyncs_only_after_lag_or_reset() {
    let backend = FakeBackend::new(LOCAL_CONTEXT);
    let view = RecordingView::new();
    let tray = loaded_tray(&backend, &view).await;
    assert_eq!(backend.count("services.status"), 1);
    let renders = view.renders();
    // A burst of 20 events: one or two renders, not 20.
    for i in 0..20 {
        let state = if i % 2 == 0 { "starting" } else { "running" };
        backend.push(vec![event_entry(
            &format!("service.{state}"),
            service_event(NGINX, "nginx", state),
        )]);
    }
    assert!(
        eventually(WAIT, || view.last().is_some_and(|r| r.icon.state
            == TrayState::Running
            && r.menu
                .find(
                    &TrayAction::ToggleService {
                        id: NGINX.into(),
                        start: false
                    }
                    .id()
                )
                .is_some()))
        .await
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    let burst = view.renders() - renders;
    assert!((1..=3).contains(&burst), "{burst} renders for 20 events");
    assert_eq!(backend.count("services.status"), 1, "events never refetch");
    // Lag and reset entries refetch the snapshot.
    backend.push(vec![StreamEntry::Lagged { missed: 3 }]);
    assert!(eventually(WAIT, || backend.count("services.status") == 2).await);
    backend.push(vec![StreamEntry::Reset]);
    assert!(eventually(WAIT, || backend.count("services.status") == 3).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(backend.count("services.status"), 3, "no polling");
    tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuilds_wait_while_the_menu_is_open() {
    let backend = FakeBackend::new(LOCAL_CONTEXT);
    let view = RecordingView::new();
    let tray = loaded_tray(&backend, &view).await;
    let renders = view.renders();
    tray.click(Click::MenuOpening);
    tokio::time::sleep(Duration::from_millis(50)).await;
    backend.push(vec![event_entry(
        "service.running",
        service_event(NGINX, "nginx", "running"),
    )]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(view.renders(), renders, "deferred while open");
    let closed = Instant::now();
    tray.click(Click::MenuClosed);
    assert!(eventually(WAIT, || view.renders() == renders + 1).await);
    assert!(closed.elapsed() < Duration::from_millis(200));
    assert_eq!(view.last().unwrap().icon.state, TrayState::Running);
    tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_starting_ring_animates_unless_motion_is_reduced() {
    let backend = FakeBackend::new(LOCAL_CONTEXT);
    let view = RecordingView::new();
    let tray = loaded_tray(&backend, &view).await;
    backend.push(vec![event_entry(
        "service.starting",
        service_event(NGINX, "nginx", "starting"),
    )]);
    assert!(eventually(WAIT, || view.rec.lock().unwrap().icons.len() >= 8).await);
    let frames: BTreeSet<u8> = view
        .rec
        .lock()
        .unwrap()
        .icons
        .iter()
        .map(|k| k.frame)
        .collect();
    assert_eq!(frames.len(), 8, "{frames:?}");
    // Reduced motion: frames stop.
    tray.set_reduced_motion(true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let count = view.rec.lock().unwrap().icons.len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(view.rec.lock().unwrap().icons.len(), count);
    // Running: no frames either.
    tray.set_reduced_motion(false);
    backend.push(vec![event_entry(
        "service.running",
        service_event(NGINX, "nginx", "running"),
    )]);
    assert!(
        eventually(WAIT, || view.last().unwrap().icon.state
            == TrayState::Running)
        .await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let count = view.rec.lock().unwrap().icons.len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(view.rec.lock().unwrap().icons.len(), count);
    tray.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appearance_picks_the_icon_variant_and_size() {
    let backend = FakeBackend::new(LOCAL_CONTEXT);
    let view = RecordingView::new();
    let tray = loaded_tray_with(
        &backend,
        &view,
        TrayAppearance {
            taskbar_light: false,
            dark_menu: true,
            scale: 1.5,
            reduced_motion: false,
        },
    )
    .await;
    let last = view.last().unwrap();
    assert!(!last.icon.taskbar_light && last.dark_menu);
    assert_eq!(last.icon.size, 24);
    tray.set_appearance(TrayAppearance {
        taskbar_light: true,
        dark_menu: false,
        scale: 2.0,
        reduced_motion: false,
    });
    assert!(eventually(WAIT, || view.last().unwrap().icon.taskbar_light).await);
    let last = view.last().unwrap();
    assert_eq!(last.icon.size, 32);
    assert!(!last.dark_menu);
    tray.stop().await;
}

// --- quit flow ------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_stop_services_sends_daemon_stop_then_exits() {
    let backend = FakeBackend::with(LOCAL_CONTEXT, Some(Duration::from_millis(80)), vec![]);
    let view = RecordingView::new();
    *view.quit.lock().unwrap() = QuitChoice::StopServices;
    let started = Instant::now();
    let outcome = quit_flow(
        backend.as_ref(),
        view.as_ref(),
        LOCAL_CONTEXT,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(outcome, QuitOutcome::StoppedAndExited(Ok(())));
    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "waited for the answer"
    );
    assert_eq!(view.rec.lock().unwrap().exits, 1);
    let calls = backend.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "daemon.stop");
    assert_eq!(calls[0].context, LOCAL_CONTEXT);
    assert_eq!(calls[0].params, Some(json!({ "keep_services": false })));
    RequestId::from_str(calls[0].request_id.as_deref().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_exits_after_the_timeout_when_the_daemon_never_answers() {
    assert_eq!(TrayOptions::default().quit_timeout, Duration::from_secs(5));
    let backend = FakeBackend::with(LOCAL_CONTEXT, None, vec![]);
    let view = RecordingView::new();
    *view.quit.lock().unwrap() = QuitChoice::StopServices;
    let started = Instant::now();
    let outcome = quit_flow(
        backend.as_ref(),
        view.as_ref(),
        LOCAL_CONTEXT,
        Duration::from_millis(300),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(matches!(outcome, QuitOutcome::StoppedAndExited(Err(ref e)) if e.code == "timeout"));
    assert!(
        elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert_eq!(view.rec.lock().unwrap().exits, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_keep_exits_without_a_call_and_cancel_stays() {
    let backend = FakeBackend::new(LOCAL_CONTEXT);
    let view = RecordingView::new();
    *view.quit.lock().unwrap() = QuitChoice::KeepServices;
    let outcome = quit_flow(
        backend.as_ref(),
        view.as_ref(),
        LOCAL_CONTEXT,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(outcome, QuitOutcome::Exited);
    assert!(backend.calls().is_empty());
    assert_eq!(view.rec.lock().unwrap().exits, 1);

    *view.quit.lock().unwrap() = QuitChoice::Cancel;
    let outcome = quit_flow(
        backend.as_ref(),
        view.as_ref(),
        LOCAL_CONTEXT,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(outcome, QuitOutcome::Cancelled);
    assert_eq!(view.rec.lock().unwrap().exits, 1);
    assert!(backend.calls().is_empty());

    // Through the menu: the actor runs the same flow on the active context.
    let backend = FakeBackend::with("hml", Some(Duration::from_millis(10)), vec![]);
    let view = RecordingView::new();
    let tray = loaded_tray(&backend, &view).await;
    *view.quit.lock().unwrap() = QuitChoice::StopServices;
    tray.menu_event("quit");
    assert!(eventually(WAIT, || view.rec.lock().unwrap().exits == 1).await);
    let stop = backend
        .calls()
        .into_iter()
        .find(|c| c.method == "daemon.stop")
        .unwrap();
    assert_eq!(stop.context, "hml");
    tray.stop().await;
}

// --- actor over a real forge-ipc server ------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn against_a_real_server_events_rebuild_the_menu_within_200_ms_without_polling() {
    let status_calls = Arc::new(AtomicUsize::new(0));
    let instances = Arc::new(Mutex::new(vec![
        instance(NGINX, "nginx", "1.27.4", "running", true),
        instance(MYSQL, "mysql", "8.4.3", "running", true),
    ]));
    let mut router = Router::new();
    {
        let (calls, instances) = (Arc::clone(&status_calls), Arc::clone(&instances));
        router.handle::<services_status, _, _>(move |_: NoParams, _ctx| {
            calls.fetch_add(1, Ordering::SeqCst);
            let body = status(instances.lock().unwrap().clone());
            async move { Ok(serde_json::from_value(body).unwrap()) }
        });
    }
    router.handle::<sites_list, _, _>(|_: NoParams, _ctx| async {
        Ok(serde_json::from_value(sites(vec![site("loja.test")])).unwrap())
    });
    router.handle::<php_list, _, _>(|_: NoParams, _ctx| async {
        Ok(serde_json::from_value(php(vec![
            php_version(PHP83, "8.3.36", true),
            php_version(PHP84, "8.4.10", false),
        ]))
        .unwrap())
    });
    let daemon = Arc::new(TestDaemon::spawn(router).await);
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());
    let view = RecordingView::new();
    let tray = Tray::spawn(
        Arc::new(bridge.clone()) as Arc<dyn TrayBackend>,
        Arc::clone(&view) as Arc<dyn TrayView>,
        LOCAL_CONTEXT,
        TrayOptions::default(),
        TrayAppearance::default(),
    );
    tray.activate();
    assert!(
        eventually(WAIT, || view
            .last()
            .is_some_and(|r| r.icon.state == TrayState::Running))
        .await,
        "{:?}",
        view.last()
    );
    assert_eq!(status_calls.load(Ordering::SeqCst), 1);
    assert_eq!(bridge.stream_count(), 1, "the tray's own events.subscribe");
    // Let the stream settle on the daemon side.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let stop_nginx = TrayAction::ToggleService {
        id: NGINX.into(),
        start: false,
    }
    .id();
    let start_nginx = TrayAction::ToggleService {
        id: NGINX.into(),
        start: true,
    }
    .id();
    let php84 = TrayAction::UsePhp("8.4.10".into()).id();
    let mut worst = Duration::ZERO;
    // Publishes, then waits for the first render that shows `id`: event-to-menu latency.
    let measure = |kind: EventKind, payload: Value, id: &str| {
        view.rec.lock().unwrap().renders.clear();
        let sent = Instant::now();
        daemon.bus.publish(kind, payload);
        let view = Arc::clone(&view);
        let id = id.to_owned();
        async move {
            assert!(
                eventually(WAIT, || view
                    .rendered_at(|r| r.menu.find(&id).is_some())
                    .is_some())
                .await,
                "{id}: {:?}",
                view.last()
            );
            view.rendered_at(|r| r.menu.find(&id).is_some()).unwrap() - sent
        }
    };
    for _ in 0..5 {
        let latency = measure(
            EventKind::ServiceStopped,
            service_event(NGINX, "nginx", "stopped"),
            &start_nginx,
        )
        .await;
        worst = worst.max(latency);
        let latency = measure(
            EventKind::ServiceRunning,
            service_event(NGINX, "nginx", "running"),
            &stop_nginx,
        )
        .await;
        worst = worst.max(latency);
    }
    // The PHP radio follows tool.current_changed.
    let sent = Instant::now();
    daemon.bus.publish(
        EventKind::ToolCurrentChanged,
        json!({ "v": 1, "tool": "php", "from": "8.3.36-nts-vs17-x64", "to": "8.4.10-nts-vs17-x64", "version_id": PHP84, "elapsed_us": 1200 }),
    );
    assert!(
        eventually(WAIT, || view.last().is_some_and(|r| r
            .menu
            .find(&php84)
            .and_then(MenuNode::checked)
            == Some(true)))
        .await
    );
    worst = worst.max(view.rec.lock().unwrap().renders.last().unwrap().0 - sent);
    println!("tray: worst event-to-menu latency {worst:?} over 11 events");
    assert!(worst < Duration::from_millis(200), "{worst:?}");
    assert_eq!(
        status_calls.load(Ordering::SeqCst),
        1,
        "no services.status besides the initial one"
    );

    // A daemon-side loss of the connection: the tray shows the error, then resyncs once.
    local.kill();
    assert!(
        eventually(WAIT, || view
            .last()
            .is_some_and(|r| r.icon.state == TrayState::Error))
        .await
    );
    assert_eq!(
        tray.model().await.unwrap().status_line(Language::PtBr),
        "Daemon indisponível"
    );
    assert!(
        eventually(Duration::from_secs(15), || status_calls
            .load(Ordering::SeqCst)
            == 2)
        .await,
        "one resync after the reconnection"
    );
    assert!(
        eventually(WAIT, || view
            .last()
            .is_some_and(|r| r.icon.state == TrayState::Running))
        .await
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(status_calls.load(Ordering::SeqCst), 2, "no polling");
    tray.stop().await;
    bridge.shutdown().await;
}
