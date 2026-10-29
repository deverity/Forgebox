//! Acceptance of M2-E1-T3 (shell API gaps, ADR-0039) against a daemon booted **in this
//! process** (`boot::boot_with`), so the test can replace the update source and the shell
//! opener: `tool.updates_available` published by the scheduler from a fake update source
//! (persisted, replayable with `resume_from`, also after a restart), `net.port_check`
//! (free port, real listener, Forgebox reservation, latency), `system.hello`
//! `not_implemented`, `daemon.status` `cpu_pct`/`rss_bytes`, `sites.open` through a
//! recording `ShellOpener` and the structured error details.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::model::port::{OwnerKind, ReservationOwner};
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, RecordingShellOpener, ShellOpen};
use forge_daemon::api::not_implemented::NOT_IMPLEMENTED;
use forge_daemon::args::Args;
use forge_daemon::boot::{self, BootOptions, Booted};
use forge_daemon::instance_lock::InstanceLock;
use forge_daemon::update_feed::testing::{FakeSource, update};
use forge_ipc::api::methods::{daemon_status, jobs_get, net_port_check, sites_add, sites_open};
use forge_ipc::api::{
    EventFilter, JobIdParams, JobState, NoParams, PortCheckParams, PortProtocol, SiteAddParams,
    SiteDto, SiteOpenParams, SiteOpenTarget, SiteRef, ToolUpdatesAvailablePayload,
};
use forge_ipc::client::Subscription;
use forge_ipc::transport::local_transport;
use forge_ipc::{CallOptions, Client, Credentials};
use forge_provider::update::DeferReason;
use futures::StreamExt;
use serde_json::json;

use common::*;

/// A daemon running inside the test process, and a client connected to it.
struct Running {
    booted: Booted,
    lock: InstanceLock,
    client: Client,
    root: ForgeRoot,
}

impl Running {
    async fn start(root: &TestRoot, options: BootOptions) -> Self {
        let root = ForgeRoot::new(platform::long_path(root.path())).unwrap();
        let lock = InstanceLock::acquire(&root).expect("instance lock");
        let config = ForgeConfig::load_or_default(&root.forgebox_toml()).unwrap();
        let report = root
            .init(InitOptions {
                case_sensitive: config.case_sensitive,
            })
            .unwrap();
        let args = Args::parse_from([
            "forgeboxd",
            "--root",
            &root.path().display().to_string(),
            "--registry",
            &repo_registry().display().to_string(),
            "--shutdown-timeout",
            "20",
        ]);
        let booted = boot::boot_with(args, root.clone(), &lock, config, report, options)
            .await
            .expect("boot");
        let client = connect_to(&root).await;
        Self {
            booted,
            lock,
            client,
            root,
        }
    }

    fn daemon(&self) -> &forge_daemon::daemon::Daemon {
        &self.booted.daemon
    }

    async fn stop(self) {
        self.client.close();
        drop(self.client);
        self.booted
            .daemon
            .shutdown
            .request(forge_daemon::daemon::ShutdownRequest {
                reason: "user".into(),
                restart: false,
                keep_services: false,
            });
        forge_daemon::shutdown::run(self.booted, self.lock).await;
    }
}

async fn connect_to(root: &ForgeRoot) -> Client {
    let credentials =
        Credentials::local(root, "shell-api-test", env!("CARGO_PKG_VERSION")).unwrap();
    Client::connect(local_transport(root), credentials, options())
        .await
        .expect("client connects")
}

async fn next_update(
    sub: &mut Subscription<forge_ipc::api::EventEnvelope>,
) -> ToolUpdatesAvailablePayload {
    let envelope = tokio::time::timeout(Duration::from_secs(10), sub.next())
        .await
        .expect("tool.updates_available in time")
        .expect("stream open");
    assert_eq!(envelope.event.kind, "tool.updates_available");
    serde_json::from_value(envelope.event.payload).expect("payload shape")
}

async fn wait_job(client: &Client, id: forge_core::ids::JobId) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return job.result.unwrap_or_default(),
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_scheduler_publishes_updates_persisted_and_replayable() {
    let root = TestRoot::new();
    let source = Arc::new(FakeSource::with(vec![
        update("php", "8.3", "8.3.36", "8.3.37"),
        update("php", "8.4", "8.4.1", "8.4.2"),
        update("nginx", "1.28", "1.28.0", "1.28.1"),
    ]));
    let running = Running::start(
        &root,
        BootOptions {
            update_source: Some(source.clone()),
            ..BootOptions::default()
        },
    )
    .await;

    // The first scheduled check of each tool runs at boot: one event per check, the last
    // one with the whole list (3 updates over 2 tools).
    let filter = EventFilter::kinds(["tool.updates_available"]);
    let mut sub = running
        .client
        .subscribe_events(filter.clone(), Some(0))
        .await
        .unwrap();
    let mut last = next_update(&mut sub).await;
    while last.count != 3 {
        last = next_update(&mut sub).await;
    }
    assert_eq!(last.v, 1);
    assert_eq!(last.updates.len(), 3);
    assert_eq!(
        (
            last.updates[0].tool.as_str(),
            last.updates[0].series.as_str(),
            last.updates[0].from.as_str(),
            last.updates[0].to.as_str()
        ),
        ("nginx", "1.28", "1.28.0", "1.28.1")
    );
    assert_eq!(
        source.checks.lock().unwrap().len(),
        2,
        "one scheduled check per tool"
    );

    // Replay with resume_from from a second connection: the same events, same seq.
    let again = connect_to(&running.root).await;
    let mut replay = again
        .subscribe_events(filter.clone(), Some(0))
        .await
        .unwrap();
    let mut replayed = next_update(&mut replay).await;
    while replayed.count != 3 {
        replayed = next_update(&mut replay).await;
    }
    assert_eq!(replayed, last);
    // And from the cursor of the live subscription: nothing newer.
    let seq = sub.last_seq().expect("a cursor");
    let mut tail = again
        .subscribe_events(filter.clone(), Some(seq))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), tail.next())
            .await
            .is_err(),
        "no event after the last seq"
    );

    // An applied update leaves the list at once; the re-check agrees (nothing more).
    source
        .found
        .lock()
        .unwrap()
        .get_mut(&forge_core::ids::ToolId::new("php").unwrap())
        .unwrap()
        .retain(|u| u.series != "8.3");
    running.daemon().bus.publish(
        forge_core::event::EventKind::ToolUpdated,
        json!({
            "v": 1, "tool": "php", "from": "8.3.36", "to": "8.3.37", "series": "8.3",
            "version_id": forge_core::ids::VersionId::new().to_string(),
        }),
    );
    let after = next_update(&mut sub).await;
    assert_eq!(after.count, 2, "{after:?}");
    assert!(
        after
            .updates
            .iter()
            .all(|u| u.series != "8.3" || u.tool.as_str() != "php")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(500), sub.next())
            .await
            .is_err(),
        "the re-check found nothing new, so nothing more is published"
    );
    again.close();
    running.stop().await;

    // The events table has them: after a restart (empty ring, a fresh bus seeded from the
    // table) `resume_from` and `events.list` still deliver the last published list.
    // The tools are still installed but their checks are deferred (metered), so only the
    // seeded list is known to the feed.
    let deferred_source = Arc::new(FakeSource::default());
    {
        let mut tools = deferred_source.tools.lock().unwrap();
        for id in ["php", "nginx"] {
            tools.push(forge_core::ids::ToolId::new(id).unwrap());
        }
    }
    *deferred_source.deferred.lock().unwrap() = Some(DeferReason::Metered);
    let restarted = Running::start(
        &root,
        BootOptions {
            update_source: Some(deferred_source),
            ..BootOptions::default()
        },
    )
    .await;
    let page = restarted
        .client
        .events_list(forge_ipc::api::EventsListParams {
            after_seq: 0,
            limit: None,
            filter: filter.clone(),
        })
        .await
        .unwrap();
    let stored: Vec<ToolUpdatesAvailablePayload> = page
        .events
        .iter()
        .map(|e| serde_json::from_value(e.payload.clone()).unwrap())
        .collect();
    assert_eq!(stored.last().map(|p| p.count), Some(2), "{stored:?}");
    assert!(stored.iter().any(|p| p.count == 3));
    let mut from_store = restarted
        .client
        .subscribe_events(filter, Some(0))
        .await
        .unwrap();
    let first = next_update(&mut from_store).await;
    assert!(first.count >= 1);

    // After the restart, with the checks deferred, an applied update publishes the whole
    // remaining list (2 pending - 1 applied), never a partial or empty one.
    let mut live = restarted
        .client
        .subscribe_events(
            EventFilter::kinds(["tool.updates_available"]),
            Some(restarted.daemon().bus.last_seq()),
        )
        .await
        .unwrap();
    restarted.daemon().bus.publish(
        forge_core::event::EventKind::ToolUpdated,
        json!({
            "v": 1, "tool": "php", "from": "8.4.1", "to": "8.4.2", "series": "8.4",
            "version_id": forge_core::ids::VersionId::new().to_string(),
        }),
    );
    let after_restart = next_update(&mut live).await;
    assert_eq!(after_restart.count, 1, "{after_restart:?}");
    assert_eq!(after_restart.updates[0].tool.as_str(), "nginx");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), live.next())
            .await
            .is_err(),
        "no further (partial or empty) publication"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_status_port_check_and_sites_open() {
    let root = TestRoot::new();
    let opener = RecordingShellOpener::new();
    let platform = platform::current().with_shell_opener(Arc::new(opener.clone()));
    let running = Running::start(
        &root,
        BootOptions {
            platform: Some(platform),
            update_source: Some(Arc::new(FakeSource::default())),
        },
    )
    .await;
    let client = &running.client;

    // system.hello: API 1.1 and the not_implemented list is the table.
    let hello = client.hello().await.unwrap();
    assert_eq!(hello.api_version, forge_ipc::api::API_VERSION);
    assert_eq!((hello.api_version.major, hello.api_version.minor), (1, 1));
    let expected: Vec<&str> = NOT_IMPLEMENTED.iter().map(|(m, _, _)| *m).collect();
    assert_eq!(hello.not_implemented, expected);
    assert!(!hello.not_implemented.iter().any(|m| m == "sites.open"));
    // Every listed method really answers not_implemented, with structured details.
    for method in &hello.not_implemented {
        let err = client
            .call_raw(method, json!({}), CallOptions::default())
            .await
            .expect_err(method);
        assert_eq!(err.code(), "not_implemented", "{method}");
        let api = err.api().unwrap();
        assert_eq!(api.reason(), Some("not_implemented"), "{method}");
        assert_eq!(api.args().unwrap()["method"], method.as_str());
        let task = NOT_IMPLEMENTED
            .iter()
            .find(|(m, _, _)| m == method)
            .map(|(_, task, _)| *task)
            .unwrap();
        assert_eq!(api.args().unwrap()["task"], task, "{method}");
        assert!(api.message.contains(&format!("roadmap {task}")), "{method}");
    }
    // A working method with a parameter of a later task says so instead of ignoring it.
    let err = client
        .call_raw(
            "logs.read",
            json!({"target": "daemon", "before_seq": 10}),
            CallOptions::default(),
        )
        .await
        .expect_err("before_seq");
    let api = err.api().unwrap();
    assert_eq!(api.code, "not_implemented");
    assert_eq!(api.args().unwrap()["task"], "M2-E3-T8");

    // daemon.status: the daemon's own CPU and memory.
    let first = client.call::<daemon_status>(NoParams {}).await.unwrap();
    assert_eq!(first.pid, std::process::id());
    assert_eq!(first.api_version.minor, 1);
    assert!(first.rss_bytes > 1_000_000, "rss {}", first.rss_bytes);
    assert!(
        (0.0..=100.0).contains(&first.cpu_pct),
        "cpu {}",
        first.cpu_pct
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    let second = client.call::<daemon_status>(NoParams {}).await.unwrap();
    assert!(
        (0.0..=100.0).contains(&second.cpu_pct),
        "cpu {}",
        second.cpu_pct
    );
    assert!(second.rss_bytes > 1_000_000);

    // net.port_check: a free port, a reservation, a real listener.
    let port = free_port();
    let check = |port: u16| async move {
        client
            .call::<net_port_check>(PortCheckParams {
                port,
                protocol: PortProtocol::Tcp,
            })
            .await
            .unwrap()
    };
    let free = check(port).await;
    assert!(free.free && free.reserved_by.is_none() && free.holder.is_none());

    let owner = ReservationOwner::new(OwnerKind::Daemon, "dns", "main").unwrap();
    running
        .daemon()
        .ports
        .reserve(port, None, owner)
        .await
        .unwrap();
    let reserved = check(port).await;
    assert!(!reserved.free);
    assert_eq!(reserved.reserved_by.as_deref(), Some("daemon/dns"));
    assert!(reserved.holder.is_none());

    let listener = std::net::TcpListener::bind(("127.0.0.1", free_port())).unwrap();
    let held = listener.local_addr().unwrap().port();
    let holder = check(held).await;
    assert!(!holder.free && holder.reserved_by.is_none());
    let holder = holder.holder.expect("the listener is reported");
    assert_eq!(holder.pid, std::process::id());
    let me = std::env::current_exe().unwrap();
    assert!(
        holder
            .exe
            .to_lowercase()
            .ends_with(&me.file_name().unwrap().to_string_lossy().to_lowercase()),
        "{} vs {}",
        holder.exe,
        me.display()
    );

    // It answers well inside 50 ms (median of 30 calls; the worst is printed).
    let mut times = Vec::new();
    for _ in 0..30 {
        let started = Instant::now();
        assert!(!check(held).await.free);
        times.push(started.elapsed());
    }
    times.sort();
    let (median, worst) = (times[times.len() / 2], times[times.len() - 1]);
    println!("net.port_check: median {median:?}, worst {worst:?}");
    assert!(median < Duration::from_millis(50), "median {median:?}");
    drop(listener);

    let err = client
        .call::<net_port_check>(PortCheckParams {
            port: 0,
            protocol: PortProtocol::Tcp,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");

    // sites.open: a site, its URL in the browser and its folder in the file manager.
    let project = root.path().join("www").join("loja");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("index.php"), "<?php echo 1;").unwrap();
    let job = client
        .call::<sites_add>(SiteAddParams {
            dir: project.display().to_string(),
            host: Some("loja.test".into()),
            php: None,
            server: None,
            secure: Some(true),
            driver: None,
        })
        .await
        .unwrap();
    let site: SiteDto = serde_json::from_value(wait_job(client, job.id).await).unwrap();
    assert_eq!(site.host, "loja.test");
    let open = |with| async move {
        client
            .call::<sites_open>(SiteOpenParams {
                site: SiteRef::from("loja.test"),
                with,
            })
            .await
    };
    assert!(open(SiteOpenTarget::Browser).await.unwrap().ok);
    assert!(open(SiteOpenTarget::Explorer).await.unwrap().ok);
    assert_eq!(
        opener.calls(),
        vec![
            ShellOpen::Url(site.url.clone()),
            ShellOpen::Path(site.root.as_deref().unwrap().into()),
        ]
    );
    assert_eq!(site.url, "https://loja.test");

    // The editor and the terminal belong to M2-E3-T9.
    for with in [SiteOpenTarget::Editor, SiteOpenTarget::Terminal] {
        let err = open(with).await.unwrap_err();
        assert_eq!(err.code(), "not_implemented");
        let api = err.api().unwrap();
        assert_eq!(api.reason(), Some("not_implemented"));
        assert_eq!(api.args().unwrap()["task"], "M2-E3-T9");
        assert_eq!(api.args().unwrap()["method"], "sites.open");
        assert!(err.to_string().contains("M2-E3-T9"), "{err}");
    }
    assert_eq!(opener.calls().len(), 2, "nothing else was opened");

    // not_found carries reason and args.
    let err = client
        .call::<sites_open>(SiteOpenParams {
            site: SiteRef::from("nope.test"),
            with: SiteOpenTarget::Browser,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    let api = err.api().unwrap();
    assert_eq!(api.reason(), Some("not_found"));
    assert!(
        api.args().unwrap()["what"]
            .as_str()
            .unwrap()
            .contains("nope.test")
    );

    running.stop().await;
}
