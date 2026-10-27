//! Typed client ↔ server round trips over the in-memory transport: at least one method
//! per API group with fake handlers, built-ins, streams, jobs, idempotency, errors,
//! roles and `local_only`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use common::{TestServer, bus, eventually, quick_config, spawn, spawn_on};
use forge_core::Timestamp;
use forge_core::ids::{ClientId, InstanceId, PortReservationId, SiteId, ToolId, VersionId};
use forge_core::jobs::{JobProgress, JobRegistry, JobSpec};
use forge_core::paths::ForgeRoot;
use forge_ipc::api::methods::*;
use forge_ipc::api::*;
use forge_ipc::auth::{Authenticator, IpcToken};
use forge_ipc::server::{EventSource, FsScope, Router, Server};
use forge_ipc::transport::{ConnectionInfo, InMemoryTransport, Transport};
use forge_ipc::{CallOptions, Client, Credentials};

fn ts() -> Timestamp {
    "2026-10-26T12:00:00Z".parse().unwrap()
}

fn tool(id: &str) -> ToolId {
    ToolId::new(id).unwrap()
}

fn site_dto(id: SiteId, host: &str) -> SiteDto {
    SiteDto {
        id,
        slug: host.replace('.', "-"),
        host: host.into(),
        aliases: vec![format!("admin.{host}")],
        kind: SiteKind::Php,
        root: Some("C:/www/loja/public".into()),
        driver: Some("laravel".into()),
        server: Some(tool("nginx")),
        php: Some(SitePhpDto {
            series: Some("8.3".into()),
            exact: None,
            resolved: Some("8.3.36".into()),
            upstream: Some("php_8_3".into()),
        }),
        upstream: None,
        websocket: false,
        secure: true,
        http_redirect: true,
        env: BTreeMap::from([("APP_ENV".into(), "local".into())]),
        xdebug: SiteXdebug::Inherit,
        parked: false,
        enabled: true,
        case_sensitive: true,
        url: format!("https://{host}"),
        processes: vec![],
        created_at: ts(),
        updated_at: ts(),
    }
}

fn pool_dto() -> PoolDto {
    PoolDto {
        series: "8.3".into(),
        version: "8.3.36".into(),
        version_id: VersionId::new(),
        state: ServiceState::Running,
        upstream: "php_8_3".into(),
        workers: 2,
        busy_workers: 0,
        min_workers: 2,
        max_workers: 12,
        ports: vec![19083, 19084],
        sites: 1,
        xdebug: false,
        requests_total: Some(10_000_000_000),
        rss_bytes: None,
    }
}

/// Every group of the method table, with the fake answers of one of its methods.
struct Fakes {
    tools: ToolListDto,
    services: ServiceStatusDto,
    logs: LogChunkDto,
    php: PhpVersionListDto,
    sites: SiteListDto,
    db: DatabaseListDto,
    net: DnsStatusDto,
    tls: CaStatusDto,
    mail: MailStatusDto,
    shell: UserPathDto,
    fs_parity: CaseSensitivityDto,
    stats: StatsDto,
    remote: RemoteClientListDto,
    registry: RegistryListDto,
}

fn fakes() -> Fakes {
    let site_id = SiteId::new();
    Fakes {
        tools: ToolListDto {
            tools: vec![ToolDto {
                id: tool("php"),
                name: "PHP".into(),
                category: Some("runtime".into()),
                description: None,
                installed: vec![ToolVersionDto {
                    id: VersionId::new(),
                    tool: tool("php"),
                    version: "8.3.36".into(),
                    canonical_name: "8.3.36-nts-vs17-x64".into(),
                    variant: BTreeMap::from([("ts".into(), "nts".into())]),
                    state: ToolVersionState::Installed,
                    source: ToolVersionSource::Registry,
                    path: "C:/Forgebox/bin/php/8.3.36-nts-vs17-x64".into(),
                    size_bytes: Some(31_457_280),
                    installed_at: Some(ts()),
                    is_current: true,
                }],
                current: Some("8.3.36".into()),
                policy: UpdatePolicy::AutoPatch,
                latest: Some("8.3.37".into()),
                update_available: Some("8.3.37".into()),
                has_service: false,
            }],
        },
        services: ServiceStatusDto {
            instances: vec![InstanceDto {
                id: InstanceId::new(),
                tool: tool("mysql"),
                version_id: VersionId::new(),
                version: "8.4.3".into(),
                name: "default".into(),
                state: ServiceState::Running,
                pid: Some(4412),
                ports: vec![PortDto {
                    id: PortReservationId::new(),
                    port: 3306,
                    protocol: PortProtocol::Tcp,
                    owner_kind: PortOwnerKind::Instance,
                    owner_id: InstanceId::new().to_string(),
                    owner_name: "mysql/default".into(),
                    name: "main".into(),
                    state: PortState::Bound,
                    conflict: None,
                }],
                autostart: true,
                started_at: Some(ts()),
                cpu_pct: Some(0.5),
                rss_bytes: Some(400 << 20),
                restarts: 0,
                last_error: None,
            }],
            pools: vec![pool_dto()],
        },
        logs: LogChunkDto {
            target: "nginx.error".into(),
            lines: vec![LogLineDto {
                seq: 41,
                ts: Some(ts()),
                source: LogSource::File,
                level: Some("error".into()),
                text: "upstream timed out".into(),
            }],
            last_seq: 41,
        },
        php: PhpVersionListDto {
            versions: vec![PhpVersionDto {
                version_id: VersionId::new(),
                version: "8.3.36".into(),
                series: "8.3".into(),
                thread_safety: "nts".into(),
                compiler: Some("vs17".into()),
                arch: "x64".into(),
                is_current_cli: true,
                pool: Some(pool_dto()),
                sites: 1,
                xdebug: XdebugMode::Trigger,
            }],
        },
        sites: SiteListDto {
            sites: vec![site_dto(site_id, "loja.test")],
        },
        db: DatabaseListDto {
            databases: vec![DatabaseDto {
                instance_id: InstanceId::new(),
                instance: "mysql/default".into(),
                name: "loja".into(),
                charset: Some("utf8mb4".into()),
                collation: None,
                size_bytes: Some(1 << 20),
                tables: Some(42),
            }],
        },
        net: DnsStatusDto {
            enabled: true,
            server_running: true,
            listen: Some("127.0.0.1:53".into()),
            tlds: vec![".test".into()],
            policy: DnsPolicyState::Applied,
        },
        tls: CaStatusDto {
            exists: true,
            subject: Some("CN=Forgebox Local CA".into()),
            thumbprint_sha256: Some("ab".repeat(32)),
            thumbprint_sha1: Some("CD".repeat(20)),
            not_after: Some(ts()),
            trusted_current_user: true,
            trusted_local_machine: false,
            name_constraints: vec![".test".into(), "127.0.0.0/8".into()],
        },
        mail: MailStatusDto {
            running: true,
            instance_id: Some(InstanceId::new()),
            smtp_port: Some(1025),
            ui_url: Some("http://127.0.0.1:8025".into()),
            messages: 3,
            unread: 1,
        },
        shell: UserPathDto {
            entries: vec!["C:/Forgebox/bin/php/current".into()],
            forgebox_entries: vec!["C:/Forgebox/bin/php/current".into()],
            missing: vec![],
        },
        fs_parity: CaseSensitivityDto {
            dir: "C:/Forgebox/www".into(),
            supported: true,
            enabled: Some(true),
            fs_type: Some("NTFS".into()),
            os_build: Some(19045),
            win_version_ok: Some(true),
            reason: None,
        },
        stats: StatsDto {
            window: StatsWindow::FiveMinutes,
            sites: vec![SiteStatsDto {
                site_id,
                host: "loja.test".into(),
                window: StatsWindow::FiveMinutes,
                at: ts(),
                requests: 1200,
                rps: 4.0,
                p50_ms: 12.5,
                p90_ms: 40.0,
                p99_ms: 120.0,
                error_pct: 0.25,
                cpu_pct: Some(3.0),
                mem_bytes: Some(64 << 20),
                approx: true,
                sampled: false,
            }],
        },
        remote: RemoteClientListDto {
            clients: vec![RemoteClientDto {
                id: ClientId::new(),
                name: "laptop".into(),
                role: Role::Operator,
                fingerprint: "SHA256:abc".into(),
                paired_at: ts(),
                last_seen_at: None,
            }],
        },
        registry: RegistryListDto {
            registries: vec![RegistryDto {
                name: "official".into(),
                url: "https://registry.forgebox.dev".into(),
                kind: RegistryKind::Http,
                official: true,
                enabled: true,
                priority: 100,
                tools: 26,
                last_refresh_at: None,
            }],
        },
    }
}

fn fake_router(f: &Fakes) -> Router {
    let mut router = Router::new();
    macro_rules! answer {
        ($m:ty, $value:expr) => {{
            let value = $value.clone();
            router.handle::<$m, _, _>(move |_, _| {
                let value = value.clone();
                async move { Ok(value) }
            });
        }};
    }
    answer!(tools_list, f.tools);
    answer!(services_status, f.services);
    answer!(logs_read, f.logs);
    answer!(php_list, f.php);
    answer!(sites_list, f.sites);
    answer!(db_list, f.db);
    answer!(net_dns_status, f.net);
    answer!(tls_ca_status, f.tls);
    answer!(mail_status, f.mail);
    answer!(shell_path_show, f.shell);
    answer!(fs_parity_case_status, f.fs_parity);
    answer!(stats_get, f.stats);
    answer!(remote_clients, f.remote);
    answer!(registry_list, f.registry);
    // A metrics stream that sends three samples.
    let sample = f.stats.sites[0].clone();
    router.handle::<metrics_subscribe, _, _>(move |_params, ctx| {
        let sample = sample.clone();
        async move {
            let sender = ctx.open_stream(None)?;
            let id = sender.id();
            for seq in 1..=3u64 {
                sender.send(&MetricsEnvelope {
                    subscription_id: id,
                    seq,
                    sample: sample.clone(),
                });
            }
            Ok(SubscriptionDto {
                subscription_id: id,
                stream: "metrics".into(),
                last_seq: 3,
            })
        }
    });
    router
}

fn temp_root() -> (tempfile::TempDir, ForgeRoot) {
    let dir = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(dir.path()).unwrap();
    for sub in ["etc/php/8.3", "logs/nginx", "www/loja", "state"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    std::fs::write(dir.path().join("state/ipc.token"), "secret").unwrap();
    (dir, root)
}

#[tokio::test]
async fn one_method_per_group_round_trips() {
    let f = fakes();
    let (_dir, root) = temp_root();
    let jobs = JobRegistry::new(bus(64));
    let job_bus = jobs.events().clone();
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.router(fake_router(&f))
            .events(EventSource::new(job_bus))
            .jobs(jobs.clone())
            .fs(FsScope::new(&root))
            .root(root.clone())
    })
    .await;
    let client = server.client();
    let mut groups = BTreeSet::new();
    let mut seen = |name: &str| {
        groups.insert(method_info(name).unwrap().group());
    };

    let status = client.daemon_status(NoParams {}).await.unwrap();
    assert_eq!(status.api_version, API_VERSION);
    assert_eq!(status.connections, 1);
    seen("daemon.status");

    assert_eq!(
        client.tools_list(ToolListParams::default()).await.unwrap(),
        f.tools
    );
    seen("tools.list");
    assert_eq!(
        client.services_status(NoParams {}).await.unwrap(),
        f.services
    );
    seen("services.status");
    let logs = client
        .logs_read(LogsReadParams {
            target: "nginx.error".into(),
            since_secs: Some(600),
            grep: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(logs, f.logs);
    seen("logs.read");
    assert_eq!(client.php_list(NoParams {}).await.unwrap(), f.php);
    seen("php.list");
    assert_eq!(client.sites_list(NoParams {}).await.unwrap(), f.sites);
    seen("sites.list");
    assert_eq!(client.db_list(DbListParams::default()).await.unwrap(), f.db);
    seen("db.list");
    assert_eq!(client.net_dns_status(NoParams {}).await.unwrap(), f.net);
    seen("net.dns_status");
    assert_eq!(client.tls_ca_status(NoParams {}).await.unwrap(), f.tls);
    seen("tls.ca_status");
    assert_eq!(client.mail_status(NoParams {}).await.unwrap(), f.mail);
    seen("mail.status");
    assert_eq!(client.shell_path_show(NoParams {}).await.unwrap(), f.shell);
    seen("shell.path_show");
    let case = client
        .fs_parity_case_status(CaseDirParams {
            dir: "C:/Forgebox/www".into(),
        })
        .await
        .unwrap();
    assert_eq!(case, f.fs_parity);
    seen("fs_parity.case_status");
    assert_eq!(
        client.stats_get(StatsParams::default()).await.unwrap(),
        f.stats
    );
    seen("stats.get");
    assert_eq!(client.remote_clients(NoParams {}).await.unwrap(), f.remote);
    seen("remote.clients");
    assert_eq!(client.registry_list(NoParams {}).await.unwrap(), f.registry);
    seen("registry.list");

    // Built-ins.
    let pong = client.system_ping(NoParams {}).await.unwrap();
    assert!(pong.server_time <= Timestamp::now());
    seen("system.ping");
    let handle = jobs.spawn(JobSpec::new("test.job"), |_| async { Ok(7) });
    handle.wait(Duration::from_secs(5)).await.unwrap();
    let listed = client.jobs_list(JobListParams::default()).await.unwrap();
    assert_eq!(listed.jobs.len(), 1);
    assert_eq!(listed.jobs[0].result, Some(json!(7)));
    seen("jobs.list");
    std::fs::write(root.path().join("etc/php/8.3/php.ini"), "memory_limit=1G\n").unwrap();
    let listing = client
        .fs_list(FsListParams {
            path: "etc/php/8.3".into(),
        })
        .await
        .unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].name, "php.ini");
    seen("fs.list");
    let page = client
        .events_list(EventsListParams::default())
        .await
        .unwrap();
    assert!(page.events.iter().any(|e| e.kind == "job.done"));
    seen("events.list");

    // Streams of a daemon-provided group (metrics) through Ctx::open_stream.
    let mut metrics = client
        .metrics_subscribe(MetricsSubscribeParams::default())
        .await
        .unwrap();
    for seq in 1..=3u64 {
        let envelope = tokio::time::timeout(Duration::from_secs(5), metrics.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(envelope.seq, seq);
        assert_eq!(envelope.sample, f.stats.sites[0]);
    }
    seen("metrics.subscribe");

    let all: BTreeSet<&str> = METHODS.iter().map(|m| m.group()).collect();
    assert_eq!(groups, all, "every API group is exercised");
}

#[tokio::test]
async fn long_running_methods_answer_with_a_job_and_report_through_events() {
    let jobs = JobRegistry::new(bus(256));
    let registry = jobs.clone();
    let mut router = Router::new();
    router.handle::<tools_install, _, _>(move |params, ctx| {
        let registry = registry.clone();
        async move {
            let specs = params.specs.clone();
            let handle = registry.spawn(
                JobSpec::new(format!("tool.install {}", specs[0])).request_id(ctx.request_id()),
                move |job| async move {
                    job.progress(JobProgress::new().pct(50.0).message("downloading"));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(ToolInstallResultDto { installed: vec![] })
                },
            );
            Ok(JobDto::from(handle.info()))
        }
    });
    let source = EventSource::new(jobs.events().clone());
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.router(router).events(source).jobs(jobs.clone())
    })
    .await;
    let client = server.client();
    let started = std::time::Instant::now();
    let job = client
        .tools_install(ToolInstallParams {
            specs: vec![ToolSpec::from("php@8.3")],
            variant: None,
            yes: true,
        })
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "answers at once"
    );
    assert_eq!(job.state, JobState::Running);
    let mut events = client
        .subscribe_events(
            EventFilter {
                job_id: Some(job.id),
                ..EventFilter::default()
            },
            Some(0),
        )
        .await
        .unwrap();
    let mut kinds = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(Duration::from_secs(5), events.next()).await
    {
        kinds.push(envelope.event.kind.clone());
        if envelope.event.kind == "job.done" {
            assert_eq!(envelope.event.payload["result"], json!({"installed": []}));
            break;
        }
    }
    assert_eq!(kinds.first().map(String::as_str), Some("job.started"));
    assert_eq!(kinds.last().map(String::as_str), Some("job.done"));
    let done = client
        .jobs_get(JobIdParams { job_id: job.id })
        .await
        .unwrap();
    assert_eq!(done.state, JobState::Done);
}

#[tokio::test]
async fn unknown_unimplemented_and_invalid_calls_have_stable_codes() {
    let mut router = Router::new();
    router.handle::<tools_info, _, _>(|_, _| async {
        Err(forge_core::ForgeError::NotFound("tool".into()))
    });
    let server = spawn(router).await;
    let client = server.client();

    let err = client
        .call_raw("nope.nope", json!({}), CallOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert_eq!(err.exit_code(), 3);

    let err = client
        .sites_add(SiteAddParams {
            dir: "C:/www/x".into(),
            host: None,
            php: None,
            server: None,
            secure: None,
            driver: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_implemented");

    let err = client
        .call_raw(
            "tools.info",
            json!({"tool": "Not A Slug"}),
            CallOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    assert_eq!(err.exit_code(), 2);

    let err = client
        .tools_info(ToolParams { tool: tool("php") })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn slow_and_panicking_handlers_do_not_break_the_connection() {
    let mut router = Router::new();
    router.handle::<tools_search, _, _>(|_, _| async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(CatalogSearchDto { entries: vec![] })
    });
    router.handle::<tools_which, _, _>(|_, _| async {
        panic!("boom");
        #[allow(unreachable_code)]
        Err(forge_core::ForgeError::Internal("unreachable".into()))
    });
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.router(router).config(quick_config())
    })
    .await;
    let client = server.client();
    let started = std::time::Instant::now();
    let err = client
        .tools_search(ToolSearchParams::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "timeout");
    assert!(started.elapsed() < Duration::from_secs(3));
    let err = client
        .tools_which(ToolWhichParams {
            tool: tool("php"),
            project_dir: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "internal");
    assert!(err.to_string().contains("boom"), "{err}");
    client.system_ping(NoParams {}).await.unwrap();
}

#[tokio::test]
async fn retries_with_the_same_request_id_execute_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut router = Router::new();
    router.handle::<sites_park, _, _>(move |params, _| {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            Ok(ParkedDirDto {
                dir: params.dir,
                tld: ".test".into(),
                sites: u32::try_from(n).unwrap(),
            })
        }
    });
    router.handle::<sites_unpark, _, _>(|_, _| async { Ok(Ack::ok()) });
    let server = spawn(router).await;
    let client = server.client();
    let params = ParkParams {
        dir: "D:/clientes".into(),
        tld: None,
    };
    let request_id = forge_core::ids::RequestId::new();
    let options = CallOptions {
        request_id: Some(request_id),
        timeout: None,
    };
    let first = client
        .call_with::<sites_park>(params.clone(), options.clone())
        .await
        .unwrap();
    // A new connection, as after a network drop.
    client.disconnect();
    let second = client
        .call_with::<sites_park>(params.clone(), options.clone())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "executed once");
    // Same id for another method is refused.
    let err = client
        .call_with::<sites_unpark>(params.clone(), options)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    // A fresh id executes again.
    let third = client.sites_park(params).await.unwrap();
    assert_eq!(third.sites, 2);
}

async fn raw(server: &TestServer) -> forge_ipc::transport::Connection {
    server.transport.connect().await.unwrap()
}

async fn exchange(conn: &mut forge_ipc::transport::Connection, frame: Value) -> Value {
    conn.sink.send(frame.to_string()).await.unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(5), conn.stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&answer).unwrap()
}

fn hello_frame(token: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "system.hello",
        "request_id": forge_core::ids::RequestId::new().to_string(),
        "params": {"token": token, "client": {"name": "raw", "version": "0"}, "api_version": {"major": 1, "minor": 0}}
    })
}

#[tokio::test]
async fn protocol_rules_hello_first_request_id_required_bad_token_closes() {
    let server = spawn(Router::new()).await;

    // Requests before hello are refused, the connection stays open.
    let mut conn = raw(&server).await;
    let answer = exchange(
        &mut conn,
        json!({"jsonrpc": "2.0", "id": 1, "method": "system.ping", "request_id": forge_core::ids::RequestId::new().to_string()}),
    )
    .await;
    assert_eq!(answer["error"]["data"]["code"], "unauthorized");
    let answer = exchange(&mut conn, hello_frame(server.token.secret())).await;
    assert_eq!(answer["result"]["role"], "admin");
    assert_eq!(answer["result"]["local"], true);

    // request_id is mandatory and must be a ULID.
    let answer = exchange(
        &mut conn,
        json!({"jsonrpc": "2.0", "id": 2, "method": "system.ping"}),
    )
    .await;
    assert_eq!(answer["error"]["code"], -32600);
    assert_eq!(answer["error"]["data"]["code"], "invalid_input");
    let answer = exchange(
        &mut conn,
        json!({"jsonrpc": "2.0", "id": 3, "method": "system.ping", "request_id": "nope"}),
    )
    .await;
    assert_eq!(answer["error"]["code"], -32600);
    let answer = exchange(
        &mut conn,
        json!({"jsonrpc": "2.0", "id": "s", "method": "nope", "request_id": forge_core::ids::RequestId::new().to_string()}),
    )
    .await;
    assert_eq!(
        (answer["id"].clone(), answer["error"]["code"].clone()),
        (json!("s"), json!(-32601))
    );
    conn.sink.send("{not json".into()).await.unwrap();
    let answer: Value = serde_json::from_str(&conn.stream.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(answer["error"]["code"], -32700);

    // A wrong token is answered, then the connection is closed.
    let mut bad = raw(&server).await;
    let answer = exchange(&mut bad, hello_frame("wrong")).await;
    assert_eq!(answer["error"]["data"]["code"], "unauthorized");
    let next = tokio::time::timeout(Duration::from_secs(5), bad.stream.next())
        .await
        .unwrap();
    assert!(next.is_none(), "connection closed after a bad token");

    // Through the client API.
    let client = Client::new(
        server.transport.clone() as Arc<dyn Transport>,
        Credentials::new(IpcToken::generate().unwrap(), "forge", "0"),
        common::fast_options(),
    );
    let err = client.system_ping(NoParams {}).await.unwrap_err();
    assert_eq!(err.code(), "unauthorized");
}

#[tokio::test]
async fn oversized_frames_close_the_connection_with_an_error() {
    // The server accepts frames up to 1 KiB; this client may send up to 64 KiB.
    let transport = InMemoryTransport::with_client_limit(true, 64 * 1024, 1024, 64 * 1024);
    let server = spawn_on(transport, |b| b).await;
    let mut conn = raw(&server).await;
    let answer = exchange(&mut conn, hello_frame(server.token.secret())).await;
    assert!(answer["result"].is_object());
    let answer = exchange(
        &mut conn,
        json!({"jsonrpc": "2.0", "id": 9, "method": "system.ping", "request_id": "x".repeat(2000)}),
    )
    .await;
    assert_eq!(answer["id"], Value::Null);
    assert_eq!(answer["error"]["code"], -32600);
    assert_eq!(answer["error"]["data"]["code"], "invalid_input");
    let next = tokio::time::timeout(Duration::from_secs(5), conn.stream.next())
        .await
        .unwrap();
    assert!(
        next.is_none(),
        "connection closed after the protocol violation"
    );
}

/// Remote connections: role from the authenticator, `local_only` refused.
struct RemoteAdmin(IpcToken);

impl Authenticator for RemoteAdmin {
    fn authenticate(&self, hello: &HelloParams, _: &ConnectionInfo) -> forge_core::Result<Role> {
        if self.0.verify(&hello.token) {
            Ok(Role::Admin)
        } else {
            Err(forge_core::ForgeError::Unauthorized("token".into()))
        }
    }
}

#[tokio::test]
async fn roles_and_local_only_are_enforced_for_remote_connections() {
    let mut router = Router::new();
    router.handle::<sites_open, _, _>(|_, _| async { Ok(Ack::ok()) });
    router.handle::<tools_policy, _, _>(|_, _| async {
        Err(forge_core::ForgeError::NotImplemented("x".into()))
    });

    // Default token authenticator: remote = viewer.
    let viewer = spawn_on(InMemoryTransport::remote(), |b| b.router(router.clone())).await;
    let client = viewer.client();
    let hello = client.hello().await.unwrap();
    assert_eq!((hello.role, hello.local), (Role::Viewer, false));
    client.system_ping(NoParams {}).await.unwrap();
    let err = client
        .tools_policy(ToolPolicyParams {
            tool: tool("php"),
            policy: UpdatePolicy::Manual,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");
    assert!(err.to_string().contains("requires role admin"), "{err}");

    // Admin over a remote transport still cannot use local_only methods.
    let token = IpcToken::generate().unwrap();
    let transport = Arc::new(InMemoryTransport::remote());
    let server = Server::builder(Arc::new(RemoteAdmin(token.clone())))
        .router(router)
        .build();
    let (handle, _task) = server.bind_and_spawn(transport.as_ref()).await.unwrap();
    let client = Client::new(
        transport as Arc<dyn Transport>,
        Credentials::new(token, "gui", "0"),
        common::fast_options(),
    );
    let params = SiteOpenParams {
        site: SiteRef::from("loja.test"),
        with: SiteOpenTarget::Editor,
    };
    let err = client.sites_open(params.clone()).await.unwrap_err();
    assert_eq!(err.code(), "forbidden");
    assert!(err.to_string().contains("local_only"), "{err}");
    handle.shutdown();

    // Locally the same call works.
    let mut local_router = Router::new();
    local_router.handle::<sites_open, _, _>(|_, _| async { Ok(Ack::ok()) });
    let local = spawn(local_router).await;
    assert!(local.client().sites_open(params).await.unwrap().ok);
}

#[tokio::test]
async fn fs_methods_are_confined_to_etc_logs_www() {
    let (dir, root) = temp_root();
    let server = spawn_on(InMemoryTransport::new(), |b| b.fs(FsScope::new(&root))).await;
    let client = server.client();

    for (path, code) in [
        ("state/ipc.token", "forbidden"),
        ("bin/php/current/php.exe", "forbidden"),
        ("etc/../state/ipc.token", "invalid_input"),
        ("/etc/php/8.3/php.ini", "invalid_input"),
        ("C:/Windows/win.ini", "invalid_input"),
        ("etc\\..\\state\\ipc.token", "invalid_input"),
    ] {
        let err = client
            .fs_read(FsReadParams {
                path: path.into(),
                offset: None,
                max_bytes: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), code, "{path}");
    }

    // A link inside www/ pointing outside the root is refused after canonicalization.
    #[cfg(windows)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
        let link = dir.path().join("www/escape");
        forge_core::platform::current()
            .link_fs()
            .create_junction(&link, outside.path())
            .unwrap();
        let err = client
            .fs_read(FsReadParams {
                path: "www/escape/secret.txt".into(),
                offset: None,
                max_bytes: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "forbidden");
        let listing = client
            .fs_list(FsListParams { path: "www".into() })
            .await
            .unwrap();
        let entry = listing.entries.iter().find(|e| e.name == "escape").unwrap();
        assert_eq!(entry.kind, FsEntryKind::Link);
    }

    // Write, read back, optimistic locking, tail.
    let written = client
        .fs_write(FsWriteParams {
            path: "etc/php/8.3/php.ini".into(),
            content: "memory_limit=1G\nupload_max_filesize=256M\n".into(),
            encoding: FsEncoding::Utf8,
            if_match: None,
            create: true,
        })
        .await
        .unwrap();
    let read = client
        .fs_read(FsReadParams {
            path: "etc/php/8.3/php.ini".into(),
            offset: None,
            max_bytes: None,
        })
        .await
        .unwrap();
    assert_eq!(read.content, "memory_limit=1G\nupload_max_filesize=256M\n");
    assert_eq!(read.etag, written.etag);
    let err = client
        .fs_write(FsWriteParams {
            path: "etc/php/8.3/php.ini".into(),
            content: "x".into(),
            encoding: FsEncoding::Utf8,
            if_match: Some("stale".into()),
            create: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    let err = client
        .fs_write(FsWriteParams {
            path: "etc/php/8.3/new.ini".into(),
            content: "x".into(),
            encoding: FsEncoding::Utf8,
            if_match: None,
            create: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");

    let log: String = (1..=500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.path().join("logs/nginx/error.log"), log).unwrap();
    let tail = client
        .fs_tail(FsTailParams {
            path: "logs/nginx/error.log".into(),
            lines: Some(3),
        })
        .await
        .unwrap();
    assert_eq!(tail.lines, ["line 498", "line 499", "line 500"]);

    // Binary content comes back as base64.
    std::fs::write(dir.path().join("www/loja/logo.bin"), [0xffu8, 0xfe, 0x00]).unwrap();
    let bin = client
        .fs_read(FsReadParams {
            path: "www/loja/logo.bin".into(),
            offset: None,
            max_bytes: None,
        })
        .await
        .unwrap();
    assert_eq!(
        (bin.encoding, bin.content.as_str()),
        (FsEncoding::Base64, "//4A")
    );
}

#[tokio::test]
async fn stream_answers_precede_their_notifications_and_unsubscribe_stops_them() {
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus(16)))
    })
    .await;
    let client = server.client();
    let sub = client
        .subscribe_events(EventFilter::default(), None)
        .await
        .unwrap();
    let id = sub.subscription_id().unwrap();
    sub.unsubscribe().await;
    let err = client
        .events_unsubscribe(UnsubscribeParams {
            subscription_id: id,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found", "already gone");
    assert!(
        eventually(Duration::from_secs(2), || server.handle.connection_count()
            == 1)
        .await
    );
}
