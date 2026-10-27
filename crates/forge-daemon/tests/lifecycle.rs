//! `forgeboxd` as a real process on a temporary root: boot, API over the named pipe
//! (`system.hello`, `daemon.status`, `events.subscribe` with replay), every method of the
//! contract answers (real backend or `not_implemented` with its roadmap task), graceful
//! stop (`daemon.shutdown` delivered, exit 0, pipe gone, lock file removed, nothing
//! left running) and the single-instance mutex.

mod common;

use std::time::{Duration, Instant};

use forge_ipc::api::methods::{
    daemon_status, events_subscribe, registry_list, services_status, sites_list, system_config_get,
    tools_list, tools_search,
};
use forge_ipc::api::{
    ConfigGetParams, EventFilter, EventsSubscribeParams, NoParams, ToolListParams, ToolSearchParams,
};
use forge_ipc::client::SubscriptionItem;
use forge_ipc::{CallOptions, ClientError};

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boots_serves_the_api_and_stops_cleanly() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, ready_in) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    println!("connected after {ready_in:?}");

    // Handshake and status.
    let hello = client.hello().await.unwrap();
    assert!(hello.local);
    assert_eq!(hello.role, forge_ipc::api::Role::Admin);
    let status = client.call::<daemon_status>(NoParams {}).await.unwrap();
    assert_eq!(status.pid, daemon.pid());
    // The daemon works with the long spelling of the root (%TEMP% is often `GUSTAV~1`).
    assert_eq!(
        status.root,
        forge_core::paths::posix_string(&forge_core::platform::long_path(root.path()))
    );
    assert!(!status.root.contains('~'), "{}", status.root);
    assert!(status.last_event_seq >= 1, "daemon.ready was published");
    let info = forge_daemon::instance_lock::read_lock_info(&root.root)
        .unwrap()
        .expect("lock file describes the daemon");
    assert_eq!(info.pid, daemon.pid());

    // Events: replay from the start includes daemon.ready (persisted and in the ring).
    let mut events = client
        .subscribe::<events_subscribe>(EventsSubscribeParams {
            filter: EventFilter::kinds(["daemon.*"]),
            resume_from: Some(0),
        })
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), events.next_item())
        .await
        .expect("replayed event")
        .expect("open subscription");
    match first {
        SubscriptionItem::Item(envelope) => assert_eq!(envelope.event.kind, "daemon.ready"),
        other => panic!("{other:?}"),
    }

    // Domain methods answer from the real components.
    let tools = client
        .call::<tools_list>(ToolListParams::default())
        .await
        .unwrap();
    assert!(
        tools.tools.iter().any(|t| t.id.as_str() == "php"),
        "catalog loaded"
    );
    let found = client
        .call::<tools_search>(ToolSearchParams {
            text: "nginx".into(),
            category: None,
        })
        .await
        .unwrap();
    assert_eq!(found.entries[0].id.as_str(), "nginx");
    let registries = client.call::<registry_list>(NoParams {}).await.unwrap();
    assert_eq!(registries.registries[0].name, "official");
    assert!(
        client
            .call::<sites_list>(NoParams {})
            .await
            .unwrap()
            .sites
            .is_empty()
    );
    assert!(
        client
            .call::<services_status>(NoParams {})
            .await
            .unwrap()
            .instances
            .is_empty()
    );
    let tld = client
        .call::<system_config_get>(ConfigGetParams { key: "tld".into() })
        .await
        .unwrap();
    assert_eq!(tld.value, ".test");

    // Every method of the contract answers: a result or a stable error, never a dropped
    // connection; methods of later tasks say so with their roadmap task.
    for method in forge_ipc::METHODS {
        // Skipped: lifecycle methods, and those that change the machine outside the root
        // (DNS policy through the elevated helper, the user PATH, the trust store).
        if matches!(
            method.name,
            "daemon.stop"
                | "daemon.restart"
                | "system.hello"
                | "net.dns_enable"
                | "net.dns_disable"
                | "shell.path_add"
                | "shell.path_remove"
                | "tls.ca_install"
        ) || method.kind == forge_ipc::api::MethodKind::Stream
        {
            continue;
        }
        let answer = client
            .call_raw(
                method.name,
                serde_json::json!({}),
                CallOptions {
                    timeout: Some(Duration::from_secs(10)),
                    ..CallOptions::default()
                },
            )
            .await;
        match answer {
            Ok(_) => {}
            Err(ClientError::Api(err)) => {
                assert!(
                    forge_core::error::ERROR_CODES.contains(&err.code.as_str()),
                    "{}: unknown code {}",
                    method.name,
                    err.code
                );
                if err.code == "not_implemented" {
                    assert!(
                        err.message.contains("roadmap M"),
                        "{}: {}",
                        method.name,
                        err.message
                    );
                }
            }
            Err(other) => panic!("{}: {other}", method.name),
        }
    }
    let share = client
        .call_raw(
            "sites.share",
            serde_json::json!({"site": "x.test"}),
            CallOptions::default(),
        )
        .await
        .unwrap_err();
    match share {
        ClientError::Api(err) => {
            assert_eq!(err.code, "not_implemented");
            assert!(err.message.contains("M3-E3"), "{}", err.message);
        }
        other => panic!("{other}"),
    }

    // Graceful stop.
    let pid = daemon.pid();
    let (exit, took) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(exit.success(), "exit {exit}:\n{}", daemon.output());
    println!("stopped in {took:?}");
    let mut saw_shutdown = false;
    while let Ok(Some(item)) =
        tokio::time::timeout(Duration::from_secs(2), events.next_item()).await
    {
        if let SubscriptionItem::Item(envelope) = item {
            if envelope.event.kind == "daemon.shutdown" {
                assert_eq!(envelope.event.payload["reason"], "user");
                saw_shutdown = true;
                break;
            }
        }
    }
    assert!(
        saw_shutdown,
        "daemon.shutdown delivered before the pipe closed"
    );
    assert!(!pipe_exists(&root.root).await, "pipe removed");
    assert!(root.lock_files().is_empty(), "{:?}", root.lock_files());
    assert!(
        root.stray_lock_files().is_empty(),
        "{:?}",
        root.stray_lock_files()
    );
    assert!(!pids_of("forgeboxd.exe").contains(&pid));
    assert!(children_of(&[pid]).is_empty(), "{:?}", children_of(&[pid]));
    let log = forge_daemon::logging::log_files(&root.root.logs());
    let text = std::fs::read_to_string(log.last().expect("daemon log file")).unwrap();
    let last = text.lines().last().unwrap();
    let json: serde_json::Value = serde_json::from_str(last).expect("JSON lines");
    assert_eq!(json["fields"]["message"], "daemon stopped");
    for line in text.lines() {
        let json: serde_json::Value = serde_json::from_str(line).expect("JSON lines");
        assert_ne!(json["level"], "ERROR", "{line}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_daemon_on_the_same_root_fails_fast() {
    let root = TestRoot::new();
    let mut first = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut first, Duration::from_secs(30)).await;

    let started = Instant::now();
    let mut second = spawn_daemon(&root, &[]);
    let status = second
        .wait_exit(Duration::from_secs(10))
        .expect("second daemon exits");
    let took = started.elapsed();
    println!("second daemon refused in {took:?}");
    assert_eq!(status.code(), Some(forge_daemon::exit::ALREADY_RUNNING));
    assert!(took < Duration::from_secs(3), "{took:?}");
    let output = second.output();
    assert!(output.contains("já existe um daemon"), "{output}");
    assert!(output.contains("Forgebox.Daemon-"), "{output}");

    // The first one is untouched.
    let status = client.call::<daemon_status>(NoParams {}).await.unwrap();
    assert_eq!(status.pid, first.pid());
    let (exit, _) = stop(&client, &mut first, Duration::from_secs(60)).await;
    assert!(exit.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_starts_a_new_daemon_on_the_same_root() {
    use forge_ipc::api::methods::daemon_restart;
    let root = TestRoot::new();
    let mut first = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut first, Duration::from_secs(30)).await;
    let old_pid = first.pid();
    client.call::<daemon_restart>(NoParams {}).await.unwrap();
    let exit = first
        .wait_exit(Duration::from_secs(60))
        .expect("old daemon exits");
    assert!(exit.success(), "{}", first.output());
    client.close();

    // The new daemon (detached, not our child) answers on the same pipe.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (client, status) = loop {
        if let Ok(credentials) = forge_ipc::Credentials::local(&root.root, "t", "0") {
            if let Ok(client) = forge_ipc::Client::connect(
                forge_ipc::transport::local_transport(&root.root),
                credentials,
                options(),
            )
            .await
            {
                if let Ok(status) = client.call::<daemon_status>(NoParams {}).await {
                    if status.pid != old_pid {
                        break (client, status);
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "no new daemon:\n{}",
            first.output()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_ne!(status.pid, old_pid);
    client
        .call::<forge_ipc::api::methods::daemon_stop>(forge_ipc::api::DaemonStopParams::default())
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while pids_of("forgeboxd.exe").contains(&status.pid) {
        assert!(Instant::now() < deadline, "restarted daemon did not stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(root.stray_lock_files().is_empty());
}
