//! Real processes (M1-E4-T5): a Node server (HTTP + WebSocket echo, only built-in modules)
//! started as a `web` process through the supervisor, on the machine's Node 20 installed
//! as `bin/nodejs/20.12.2`.
//!
//! Needs Node 20 in `C:\Program Files\node-20` (the tests skip with a message otherwise).

#![cfg(windows)]

mod support;

use std::time::Duration;

use forge_core::model::port::OwnerKind;
use forge_supervisor::InstanceState;
use serde_json::Value;
use support::{harness, http_get, is_listening, wait_until, ws_echo};

fn env_of(port: u16, host: &str) -> Value {
    let response = http_get(port, host, "/env");
    assert_eq!(response.status, 200, "{response:?}");
    serde_json::from_str(&response.body).unwrap_or_else(|e| panic!("{e}: {}", response.body))
}

fn paths_equal(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").to_ascii_lowercase();
    norm(a) == norm(b)
}

#[tokio::test(flavor = "multi_thread")]
async fn node_web_process_serves_http_and_websocket_and_stop_frees_the_port() {
    let Some(h) = harness().await else { return };
    let dir = h.project("app");
    let site = h.site(
        "app.test",
        &dir,
        "web: { cmd: 'node server.js', port: auto, proxy: true, runtime: 'nodejs@20' }",
    );

    let status = h.manager.up(&site).await.unwrap();
    assert!(status.ready(), "{status:#?}");
    let web = status.process("web").unwrap();
    assert_eq!(web.instance.state, InstanceState::Running);
    assert_eq!(web.runtime.as_deref(), Some("nodejs 20.12.2"));
    let port = web.port.expect("an allocated port");
    assert!(
        (3000..=3999).contains(&port),
        "{port} is outside the apps range"
    );
    let pid = web.instance.pid.expect("pid");
    println!("web: pid {pid} listening on {port}");

    // HTTP.
    let response = http_get(port, "app.test", "/");
    assert_eq!(
        (response.status, response.body.as_str()),
        (200, "hello from forgebox app\n")
    );
    // The environment the process really received.
    let seen = env_of(port, "app.test");
    assert_eq!(seen["version"], "v20.12.2");
    assert_eq!(seen["env"]["PORT"], port.to_string());
    assert_eq!(seen["env"]["FORGEBOX_SITE"], "app.test");
    assert_eq!(seen["env"]["APP_ENV"], "local");
    let root = h.root.to_posix_string();
    assert_eq!(
        seen["env"]["NODE_EXTRA_CA_CERTS"],
        format!("{root}/certs/ca/forgebox-ca.crt")
    );
    assert_eq!(
        seen["env"]["npm_config_prefix"],
        format!("{root}/data/node/20.12.2/npm-global")
    );
    // PATH: the runtime folder and the system folders, nothing of the user's.
    let mut expected = vec![h.root.bin().join("nodejs").join("20.12.2")];
    expected.extend(h.supervisor.jobs().processes().system_path());
    let expected = std::env::join_paths(&expected)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let path = seen["env"]["PATH"].as_str().unwrap();
    assert!(
        paths_equal(path, &expected),
        "PATH {path:?} != {expected:?}"
    );
    // WebSocket (the transport of HMR).
    assert_eq!(
        ws_echo(port, "app.test", "/_next/webpack-hmr", "hello hmr"),
        "hello hmr"
    );
    assert_eq!(
        ws_echo(port, "app.test", "/", &"x".repeat(100)),
        "x".repeat(100)
    );

    // Output goes to logs/apps/<host>/<name>.log.
    let log = h.root.logs().join("apps").join("app.test").join("web.log");
    assert!(
        wait_until(Duration::from_secs(5), || std::fs::read_to_string(&log)
            .is_ok_and(|text| text.contains(&format!("listening on {port}")))),
        "log {} missing the startup line",
        log.display()
    );

    // On-visit hook of a site without on-visit processes starts nothing.
    let outcome = h.manager.ensure_started("app.test").await.unwrap();
    assert!(
        outcome.started.is_empty() && outcome.already.is_empty(),
        "{outcome:?}"
    );

    // Stop: the process and its port are gone, and the Job is empty.
    let status = h
        .manager
        .stop_process("app.test", Some("web"))
        .await
        .unwrap();
    assert_eq!(
        status.process("web").unwrap().instance.state,
        InstanceState::Stopped
    );
    assert!(
        wait_until(Duration::from_secs(5), || !is_listening(port)),
        "port {port} is still open after stop"
    );
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "port {port} cannot be bound"
    );
    assert!(h.supervisor.jobs().pids().unwrap().is_empty());

    // Start again: same port (the reservation is stable), new process.
    let status = h
        .manager
        .start_process("app.test", Some("web"))
        .await
        .unwrap();
    let again = status.process("web").unwrap();
    assert_eq!(again.port, Some(port));
    assert_ne!(again.instance.pid, Some(pid));
    assert_eq!(http_get(port, "app.test", "/").status, 200);

    // Down: unregistered, port free, nothing left. Forget frees the reservation.
    let site_id = site.id;
    assert_eq!(h.manager.down("app.test").await.unwrap(), ["web"]);
    assert_eq!(
        h.manager.status("app.test").await.unwrap_err().code(),
        "not_found"
    );
    assert!(wait_until(Duration::from_secs(5), || !is_listening(port)));
    assert!(h.supervisor.jobs().pids().unwrap().is_empty());
    assert_eq!(h.allocator.list().await.unwrap().len(), 1);
    h.manager.forget(site_id, "app.test").await.unwrap();
    let left: Vec<_> = h
        .allocator
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.owner.kind == OwnerKind::App)
        .collect();
    assert!(left.is_empty(), "{left:?}");
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn npm_scripts_go_through_cmd_exe_and_die_with_their_job() {
    let Some(h) = harness().await else { return };
    let dir = h.project("npmapp");
    // `npm` is npm.cmd: started through cmd.exe, resolved only in the runtime folder.
    let site = h.site(
        "npm.test",
        &dir,
        "web: { cmd: 'npm run dev -- --flag \"two words\"', port: auto, proxy: true }",
    );
    let status = h.manager.up(&site).await.unwrap();
    let web = status.process("web").unwrap();
    assert_eq!(web.instance.state, InstanceState::Running, "{status:#?}");
    assert_eq!(
        web.runtime.as_deref(),
        Some("nodejs 20.12.2"),
        "implied by the npm command"
    );
    let port = web.port.unwrap();
    let seen = env_of(port, "npm.test");
    assert_eq!(seen["version"], "v20.12.2");
    assert_eq!(seen["env"]["PORT"], port.to_string());
    // The quotes of the command line reached `npm` intact.
    assert_eq!(seen["argv"], serde_json::json!(["--flag", "two words"]));
    assert_eq!(ws_echo(port, "npm.test", "/", "through npm"), "through npm");

    h.manager.down("npm.test").await.unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || !is_listening(port)),
        "port {port} still open"
    );
    assert!(
        h.supervisor.jobs().pids().unwrap().is_empty(),
        "npm/cmd/node left behind"
    );
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn version_file_selects_the_runtime_when_none_is_declared() {
    let Some(h) = harness().await else { return };
    // A newer runtime without binaries: it would win without the version file.
    std::fs::create_dir_all(h.root.bin().join("nodejs").join("22.99.0")).unwrap();
    let dir = h.project("pinned");
    std::fs::write(dir.join(".nvmrc"), "# pinned\nv20\n").unwrap();
    let site = h.site(
        "pinned.test",
        &dir,
        "web: { cmd: 'node server.js', proxy: true }",
    );
    let status = h.manager.up(&site).await.unwrap();
    let web = status.process("web").unwrap();
    assert_eq!(web.instance.state, InstanceState::Running, "{status:#?}");
    assert_eq!(
        env_of(web.port.unwrap(), "pinned.test")["version"],
        "v20.12.2"
    );

    // Without the file the newest installed version is chosen, and it has no node.exe.
    h.manager.down("pinned.test").await.unwrap();
    std::fs::remove_file(dir.join(".nvmrc")).unwrap();
    let err = h.manager.up(&site).await.unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
    // A selector that matches nothing is an error that lists what is installed.
    let site = h.site(
        "pinned.test",
        &dir,
        "web: { cmd: 'node server.js', proxy: true, runtime: 'nodejs@18' }",
    );
    let err = h.manager.up(&site).await.unwrap_err();
    assert!(err.to_string().contains("20.12.2"), "{err}");
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn on_visit_processes_start_only_on_the_first_ensure_started() {
    let Some(h) = harness().await else { return };
    let dir = h.project("lazy");
    let site = h.site(
        "lazy.test",
        &dir,
        "web: { cmd: 'node server.js', proxy: true, runtime: 'nodejs@20', autostart: on-visit }\n\
         worker: { cmd: 'node server.js 3999', runtime: 'nodejs@20', autostart: false }",
    );
    let status = h.manager.up(&site).await.unwrap();
    let web = status.process("web").unwrap();
    let port = web.port.unwrap();
    // Registered, not started: nothing listens.
    assert_eq!(web.instance.state, InstanceState::Stopped);
    assert_eq!(
        status.process("worker").unwrap().instance.state,
        InstanceState::Stopped
    );
    assert!(!is_listening(port));

    // The page the daemon serves on a 502: first visit starts the process.
    let page = h.manager.starting_page("lazy.test", "pt-BR").await.unwrap();
    assert_eq!(page.status, 503);
    assert_eq!(page.retry_after_secs, 2);
    assert!(page.html.contains("Iniciando"), "{}", page.html);
    assert!(page.html.contains(r#"http-equiv="refresh""#));
    assert!(page.html.contains("lazy.test"));
    let english = h
        .manager
        .starting_page("lazy.test", "en-US,en;q=0.9")
        .await
        .unwrap();
    assert!(
        english.html.contains("Starting your app"),
        "{}",
        english.html
    );

    let status = h
        .manager
        .wait_ready("lazy.test", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(status.ready(), "{status:#?}");
    let web = status.process("web").unwrap();
    assert_eq!(
        web.instance.runs, 1,
        "the two page loads started the process once"
    );
    assert!(web.visit_started);
    assert_eq!(http_get(port, "lazy.test", "/").status, 200);
    assert_eq!(
        status.process("worker").unwrap().instance.state,
        InstanceState::Stopped,
        "autostart: false is never started by a visit"
    );

    // Later calls start nothing.
    let outcome = h.manager.ensure_started("lazy.test").await.unwrap();
    assert!(outcome.started.is_empty(), "{outcome:?}");
    assert_eq!(outcome.already, ["web"]);
    let page = h.manager.starting_page("lazy.test", "pt-BR").await.unwrap();
    assert!(page.ready);
    assert_eq!(
        h.manager
            .status("lazy.test")
            .await
            .unwrap()
            .process("web")
            .unwrap()
            .instance
            .runs,
        1
    );

    // The user stopped it: a visit does not bring it back.
    h.manager
        .stop_process("lazy.test", Some("web"))
        .await
        .unwrap();
    let outcome = h.manager.ensure_started("lazy.test").await.unwrap();
    assert!(outcome.started.is_empty(), "{outcome:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!is_listening(port));
    // Unknown hosts are not apps.
    assert_eq!(
        h.manager
            .ensure_started("other.test")
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sites_run_side_by_side_each_in_its_own_job() {
    let Some(h) = harness().await else { return };
    let dir_a = h.project("alpha");
    let dir_b = h.project("beta");
    let a = h.site(
        "alpha.test",
        &dir_a,
        "web: { cmd: 'node server.js', proxy: true, runtime: 'nodejs@20' }",
    );
    let b = h.site(
        "beta.test",
        &dir_b,
        "web: { cmd: 'node server.js', proxy: true, runtime: 'nodejs@20' }",
    );
    let status_a = h.manager.up(&a).await.unwrap();
    let status_b = h.manager.up(&b).await.unwrap();
    let port_a = status_a.process("web").unwrap().port.unwrap();
    let port_b = status_b.process("web").unwrap().port.unwrap();
    assert_ne!(port_a, port_b);
    assert_eq!(
        env_of(port_a, "alpha.test")["env"]["FORGEBOX_SITE"],
        "alpha.test"
    );
    assert_eq!(
        env_of(port_b, "beta.test")["env"]["FORGEBOX_SITE"],
        "beta.test"
    );
    assert!(paths_equal(
        env_of(port_a, "alpha.test")["cwd"].as_str().unwrap(),
        dir_a.to_str().unwrap()
    ));
    assert_eq!(
        h.manager.hosts().await.unwrap(),
        ["alpha.test", "beta.test"]
    );

    // Down for one site leaves the other alone.
    h.manager.down("alpha.test").await.unwrap();
    assert!(wait_until(Duration::from_secs(5), || !is_listening(port_a)));
    assert!(is_listening(port_b));
    assert_eq!(http_get(port_b, "beta.test", "/").status, 200);

    // Everything goes with shutdown.
    h.manager.shutdown().await;
    assert!(wait_until(Duration::from_secs(5), || !is_listening(port_b)));
    assert!(h.supervisor.jobs().pids().unwrap().is_empty());
}
