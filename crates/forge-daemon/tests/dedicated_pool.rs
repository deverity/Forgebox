//! Dedicated PHP pool per site (M2-E5-T11, docs/06 §2.1, ADR-0044 §7) against the real
//! `forgeboxd` with the fake PHP (`tests/fixtures/fake_php.rs`, a FastCGI `php-cgi.exe`
//! that answers `forgebox-health <version> cgi-fcgi <pid>`) and the fake nginx
//! (`tests/fixtures/fake_nginx.rs`, which only holds its ports). The test plays the web
//! server: before each request it reads what nginx would load after its reload — the
//! `fastcgi_pass` of `etc/nginx/sites/<host>.conf` and the servers of
//! `etc/nginx/upstreams/<name>.conf` — and sends a FastCGI request to one of them.
//!
//! * `sites.pool_mode` shared → dedicated → shared while requests keep coming to the
//!   switched site and to another site of the same version: no request fails, the vhost
//!   moves to `php_8_4__loja-test` and back, the other site stays on `php_8_4`, two pools
//!   of the version while dedicated, then the dedicated pool stops and its ports go back;
//!   while dedicated, `stats.get` reports the site's CPU/RAM as exact (`approx = false`).
//! * the update engine's `PoolSwitcher` (a `tools.rollback php@8.4` from 8.4.27 to the
//!   retained 8.4.26) switches the shared **and** the dedicated pool under load.
//! * ignored, online: the same toggle with the official nginx 1.30.6 and PHP 8.4.27 under
//!   HTTP load.
#![cfg(windows)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use forge_fcgi::probe::{FcgiRequest, request};
use forge_ipc::Client;
use forge_ipc::api::methods::{
    jobs_get, php_list, php_pool_status, services_ports, services_status, sites_list,
    sites_pool_mode, stats_get, tools_rollback,
};
use forge_ipc::api::{
    JobDto, JobIdParams, JobState, NoParams, PhpPoolStatusParams, PoolDto, ServiceState, SiteDto,
    SitePoolMode, SitePoolModeParams, SiteRef, StatsParams, StatsWindow, ToolRollbackParams,
    ToolSpec,
};

use common::*;

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

fn fake_nginx() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_nginx")).clone()
}

/// A root with the fake PHP `versions`, the fake nginx and the parked sites `loja.test`
/// and `blog.test` (`www/<name>/index.php`, generic PHP on the newest series).
fn root_with(versions: &[&str]) -> TestRoot {
    let root = TestRoot::new();
    for version in versions {
        let dir = root.root.bin().join("php").join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
        std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    }
    let nginx = root.root.bin().join("nginx").join("1.30.6");
    std::fs::create_dir_all(&nginx).unwrap();
    std::fs::copy(fake_nginx(), nginx.join("nginx.exe")).unwrap();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    // The file ends inside `[ports.ranges]`.
    let toml = format!("{toml}web = [{}, {}]\n", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    for site in ["loja", "blog"] {
        let dir = root.root.www().join(site);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.php"), "<?php echo 'ok';\n").unwrap();
    }
    root
}

/// What nginx would route the PHP of `host` to after a reload: the `fastcgi_pass` name of
/// its vhost and the ports of that upstream (`None` while a file is missing or half
/// written).
fn route(root: &Path, host: &str) -> Option<(String, Vec<u16>)> {
    let etc = root.join("etc").join("nginx");
    let site = std::fs::read_to_string(etc.join("sites").join(format!("{host}.conf"))).ok()?;
    let name = site.lines().map(str::trim).find_map(|l| {
        l.strip_prefix("fastcgi_pass ")
            .map(|n| n.trim_end_matches(';').to_owned())
    })?;
    let upstream =
        std::fs::read_to_string(etc.join("upstreams").join(format!("{name}.conf"))).ok()?;
    let ports: Vec<u16> = upstream
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("server 127.0.0.1:"))
        .filter_map(|p| p.trim_end_matches(';').parse().ok())
        .collect();
    (!ports.is_empty()).then_some((name, ports))
}

/// Pause between two requests of a load task (at most 50 requests/s each). Without it the
/// fake answers ~600 requests/s per worker and outruns the pool's request estimate (a TCP
/// table sample per second), so a worker reaches php-cgi's own hard limit before the
/// recycle replaces it: a fake-only artefact unrelated to the pool mode. Kept light so the
/// latency budgets of the binaries the runner schedules next to this one (metrics.rs,
/// `services.status` p99 < 50 ms with 400 sites) do not pay for this load: every switch
/// still sees continuous requests on every route.
const PACE: Duration = Duration::from_millis(20);

/// What one load task saw.
#[derive(Debug, Default)]
struct Load {
    ok: u32,
    failed: Vec<String>,
    upstreams: BTreeSet<String>,
    /// PHP versions that answered (from the fake's body).
    versions: BTreeSet<String>,
}

/// Requests to `host` until `stop`, one after the other, each to the route nginx would
/// use right then (round robin over its servers).
fn load(root: PathBuf, host: &'static str, stop: Arc<AtomicBool>) -> tokio::task::JoinHandle<Load> {
    tokio::spawn(async move {
        let docroot = root.join("www").join(host.trim_end_matches(".test"));
        let script = docroot.join("index.php");
        let req = FcgiRequest::get("/index.php", &script, &docroot);
        let mut out = Load::default();
        let mut turn = 0usize;
        while !stop.load(Ordering::Relaxed) {
            let Some((name, ports)) = route(&root, host) else {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            };
            turn += 1;
            let port = ports[turn % ports.len()];
            let addr = format!("127.0.0.1:{port}").parse().unwrap();
            match request(addr, &req, Duration::from_secs(10)).await {
                Ok(answer) if answer.status == 200 => {
                    out.ok += 1;
                    if let Some(version) = answer.body_text().split_whitespace().nth(1) {
                        out.versions.insert(version.to_owned());
                    }
                }
                Ok(answer) => out
                    .failed
                    .push(format!("{name} {port}: status {}", answer.status)),
                Err(err) => out.failed.push(format!("{name} {port}: {err}")),
            }
            out.upstreams.insert(name);
            tokio::time::sleep(PACE).await;
        }
        out
    })
}

/// Load on `loja.test` (two tasks) and `blog.test` (one) until [`Loads::stop`].
struct Loads {
    stop: Arc<AtomicBool>,
    tasks: Vec<(&'static str, tokio::task::JoinHandle<Load>)>,
}

impl Loads {
    fn start(root: &TestRoot) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let tasks = ["loja.test", "loja.test", "blog.test"]
            .into_iter()
            .map(|host| {
                (
                    host,
                    load(root.path().to_path_buf(), host, Arc::clone(&stop)),
                )
            })
            .collect();
        Self { stop, tasks }
    }

    /// Per host: the merged results.
    async fn stop(self) -> BTreeMap<&'static str, Load> {
        self.stop.store(true, Ordering::Relaxed);
        let mut out: BTreeMap<&'static str, Load> = BTreeMap::new();
        for (host, task) in self.tasks {
            let one = task.await.unwrap();
            let merged = out.entry(host).or_default();
            merged.ok += one.ok;
            merged.failed.extend(one.failed);
            merged.upstreams.extend(one.upstreams);
            merged.versions.extend(one.versions);
        }
        out
    }
}

async fn finish(client: &Client, job: JobDto) -> JobDto {
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        let now = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if now.state.is_terminal() {
            return now;
        }
        assert!(Instant::now() < deadline, "job {} still running", job.id);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn pool_mode(client: &Client, host: &str, mode: SitePoolMode) -> SiteDto {
    let job = client
        .call::<sites_pool_mode>(SitePoolModeParams {
            site: SiteRef::from(host),
            mode,
        })
        .await
        .unwrap();
    let done = finish(client, job).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    serde_json::from_value(done.result.expect("the site")).unwrap()
}

async fn site(client: &Client, host: &str) -> SiteDto {
    client
        .call::<sites_list>(NoParams {})
        .await
        .unwrap()
        .sites
        .into_iter()
        .find(|s| s.host == host)
        .unwrap_or_else(|| panic!("{host} not listed"))
}

async fn pools(client: &Client) -> Vec<PoolDto> {
    client
        .call::<php_pool_status>(PhpPoolStatusParams { version: None })
        .await
        .unwrap()
        .pools
}

fn upstream_of(site: &SiteDto) -> Option<String> {
    site.php.as_ref().and_then(|p| p.upstream.clone())
}

/// Boots the daemon and waits for nginx and both sites served by `version`.
async fn up(root: &TestRoot, version: &str) -> (DaemonProcess, Client) {
    let mut daemon = spawn_daemon(root, &[]);
    let (client, _) = connect(root, &mut daemon, Duration::from_secs(60)).await;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        let nginx = status
            .instances
            .iter()
            .any(|i| i.tool.as_str() == "nginx" && i.state == ServiceState::Running);
        let served = ["loja.test", "blog.test"]
            .iter()
            .all(|h| route(root.path(), h).is_some_and(|(name, _)| name == "php_8_4"));
        let pool = status
            .pools
            .iter()
            .any(|p| p.version == version && p.state == ServiceState::Running);
        if nginx && served && pool {
            return (daemon, client);
        }
        assert!(
            Instant::now() < deadline,
            "not served: {status:#?}\n{}",
            daemon_log(root)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits until `pred` holds over the pools (or 60 s).
async fn wait_pools(
    client: &Client,
    what: &str,
    pred: impl Fn(&[PoolDto]) -> bool,
) -> Vec<PoolDto> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let now = pools(client).await;
        if pred(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "{what}: {now:#?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_dedicated_shared_under_load_loses_no_request() {
    let root = root_with(&["8.4.27"]);
    let (mut daemon, client) = up(&root, "8.4.27").await;
    assert_eq!(
        site(&client, "loja.test").await.pool_mode,
        SitePoolMode::Shared
    );
    let loads = Loads::start(&root);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // shared → dedicated: a pool of its own, the other site untouched.
    let started = Instant::now();
    let loja = pool_mode(&client, "loja.test", SitePoolMode::Dedicated).await;
    println!("dedicated in {:?}", started.elapsed());
    assert_eq!(loja.pool_mode, SitePoolMode::Dedicated);
    assert_eq!(upstream_of(&loja).as_deref(), Some("php_8_4__loja-test"));
    assert_eq!(
        route(root.path(), "loja.test").map(|r| r.0).as_deref(),
        Some("php_8_4__loja-test")
    );
    assert_eq!(
        route(root.path(), "blog.test").map(|r| r.0).as_deref(),
        Some("php_8_4"),
        "the other site keeps the shared pool"
    );
    let blog = site(&client, "blog.test").await;
    assert_eq!(upstream_of(&blog).as_deref(), Some("php_8_4"));
    let two = wait_pools(&client, "two pools of 8.4.27", |p| {
        p.iter()
            .filter(|p| p.version == "8.4.27" && p.state == ServiceState::Running)
            .count()
            == 2
    })
    .await;
    let dedicated = two
        .iter()
        .find(|p| p.upstream == "php_8_4__loja-test")
        .expect("the dedicated pool")
        .clone();
    let shared = two.iter().find(|p| p.upstream == "php_8_4").unwrap();
    assert_eq!((dedicated.sites, shared.sites), (1, 1), "{two:#?}");
    assert!(!dedicated.ports.is_empty());
    assert!(
        dedicated.ports.iter().all(|p| !shared.ports.contains(p)),
        "{two:#?}"
    );

    // Exact attribution while dedicated: the pool's CPU/RAM go to the site alone.
    let deadline = Instant::now() + Duration::from_secs(45);
    let exact = loop {
        let stats = client
            .call::<stats_get>(StatsParams {
                site: Some(SiteRef::from("loja.test")),
                window: StatsWindow::OneMinute,
                ..StatsParams::default()
            })
            .await
            .unwrap();
        let s = stats.sites.into_iter().next().expect("loja.test stats");
        if s.cpu_pct.is_some() && s.workers_active.is_some_and(|w| w > 0) {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "no resources for loja.test: {s:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert!(!exact.approx, "a dedicated site is exact: {exact:?}");
    assert!(
        exact.processes.is_some_and(|n| n >= 1),
        "its workers are its own processes: {exact:?}"
    );

    // dedicated → shared: the dedicated pool drains, stops and gives its ports back.
    let started = Instant::now();
    let loja = pool_mode(&client, "loja.test", SitePoolMode::Shared).await;
    println!("shared again in {:?}", started.elapsed());
    assert_eq!(loja.pool_mode, SitePoolMode::Shared);
    assert_eq!(upstream_of(&loja).as_deref(), Some("php_8_4"));
    assert_eq!(
        route(root.path(), "loja.test").map(|r| r.0).as_deref(),
        Some("php_8_4")
    );
    wait_pools(&client, "the dedicated pool stops", |p| {
        p.iter().all(|p| p.upstream != "php_8_4__loja-test")
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ports = client.call::<services_ports>(NoParams {}).await.unwrap();
        let held: Vec<u16> = ports
            .ports
            .iter()
            // A freed port may be handed to the shared pool again; the owner tells.
            .filter(|p| p.owner_id.contains('~'))
            .map(|p| p.port)
            .collect();
        if held.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ports of the dedicated pool still reserved: {held:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let seen = loads.stop().await;
    for (host, load) in &seen {
        println!(
            "{host}: {} ok, {} failed, upstreams {:?}: {:?}",
            load.ok,
            load.failed.len(),
            load.upstreams,
            load.failed
        );
    }
    for (host, load) in &seen {
        assert!(load.failed.is_empty(), "{host}: {:?}", load.failed);
        assert!(load.ok > 20, "{host}: {load:?}");
    }
    assert_eq!(
        seen["loja.test"].upstreams,
        BTreeSet::from(["php_8_4".to_owned(), "php_8_4__loja-test".to_owned()])
    );
    assert_eq!(
        seen["blog.test"].upstreams,
        BTreeSet::from(["php_8_4".to_owned()])
    );
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pool_switcher_moves_the_shared_and_the_dedicated_pool() {
    let root = root_with(&["8.4.26", "8.4.27"]);
    // A completed update 8.4.26 → 8.4.27 with 8.4.26 retained (what `tools.update` leaves),
    // recorded with the ids the first boot discovered.
    let (mut daemon, client) = up(&root, "8.4.27").await;
    let versions = client.call::<php_list>(NoParams {}).await.unwrap().versions;
    let id_of = |v: &str| {
        versions
            .iter()
            .find(|r| r.version == v)
            .unwrap_or_else(|| panic!("{v} not discovered: {versions:?}"))
            .version_id
    };
    let (old, new) = (id_of("8.4.26"), id_of("8.4.27"));
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    {
        use forge_core::store::{
            NewUpdateHistory, Store, UpdateHistoryRepo, UpdateStatus, UpdateTrigger,
        };
        let store = Store::open(&root.root.db()).unwrap();
        UpdateHistoryRepo::new(store.conn())
            .insert(&NewUpdateHistory {
                tool_id: forge_core::ids::ToolId::new("php").unwrap(),
                series: Some("8.4".into()),
                from_version_id: Some(old),
                to_version_id: Some(new),
                from_version: Some("8.4.26".into()),
                to_version: "8.4.27".into(),
                policy: None,
                triggered_by: UpdateTrigger::User,
                status: UpdateStatus::Completed,
                job_id: None,
                rollback_of: None,
            })
            .unwrap();
        store.close().unwrap();
    }
    let (mut daemon, client) = up(&root, "8.4.27").await;
    let loja = pool_mode(&client, "loja.test", SitePoolMode::Dedicated).await;
    assert_eq!(
        loja.php.as_ref().unwrap().resolved.as_deref(),
        Some("8.4.27")
    );
    let loads = Loads::start(&root);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let job = client
        .call::<tools_rollback>(ToolRollbackParams {
            spec: ToolSpec::from("php@8.4"),
        })
        .await
        .unwrap();
    let done = finish(&client, job).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    let now = wait_pools(&client, "both pools on 8.4.26", |p| {
        let running: Vec<&PoolDto> = p
            .iter()
            .filter(|p| p.state == ServiceState::Running)
            .collect();
        running.len() == 2 && running.iter().all(|p| p.version == "8.4.26")
    })
    .await;
    let names: BTreeSet<&str> = now.iter().map(|p| p.upstream.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from(["php_8_4", "php_8_4__loja-test"]),
        "{now:#?}"
    );
    let loja = site(&client, "loja.test").await;
    assert_eq!(
        loja.php.as_ref().unwrap().resolved.as_deref(),
        Some("8.4.26")
    );
    assert_eq!(upstream_of(&loja).as_deref(), Some("php_8_4__loja-test"));
    tokio::time::sleep(Duration::from_millis(500)).await;

    let seen = loads.stop().await;
    for (host, load) in &seen {
        println!(
            "{host}: {} ok, {} failed, versions {:?}: {:?}",
            load.ok,
            load.failed.len(),
            load.versions,
            load.failed
        );
    }
    for (host, load) in &seen {
        assert!(load.failed.is_empty(), "{host}: {:?}", load.failed);
        assert!(
            load.versions.contains("8.4.26"),
            "{host} served by the retained version after the switch: {load:?}"
        );
    }
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

/// `GET /` with `Host: host` through the real nginx (HTTP/1.0, the whole answer).
async fn http_get(port: u16, host: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(stream) => stream,
        Err(err) => return format!("connect: {err}"),
    };
    let request = format!("GET / HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if let Err(err) = stream.write_all(request.as_bytes()).await {
        return format!("write: {err}");
    }
    let mut out = String::new();
    if let Err(err) = stream.read_to_string(&mut out).await {
        return format!("read: {err}");
    }
    out
}

/// A connection nginx closed before sending any byte. On a reload nginx's old workers
/// close the connections whose request they have not read yet (graceful shutdown); with
/// several clients a few of those happen on any reload, `services.reload` alone included
/// (measured while writing this test), and browsers retry them. Anything else (a 502/504
/// from a pool that went away, a wrong PHP) is a real failure.
fn is_reload_drop(answer: &str) -> bool {
    answer.is_empty()
        || ["connect:", "write:", "read:"]
            .iter()
            .any(|what| answer.starts_with(what))
}

/// The same toggle with the official binaries (`cargo test -p forge-daemon --test
/// dedicated_pool -- --ignored --nocapture`): nginx 1.30.6 in front of PHP 8.4.27, two
/// sites in the store, HTTP load on both (four clients on `loja.test`, one on
/// `blog.test`) while `loja.test` goes dedicated and back. No request may fail (a
/// connection dropped by an nginx reload before any byte is retried once,
/// [`is_reload_drop`]); the dedicated pool's `php-cgi.exe` processes are gone afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the official nginx 1.30.6 and PHP 8.4.27 zips (shared cache) and VC++ 2022"]
async fn dedicated_pool_toggle_under_http_load_online() {
    use forge_core::model::port::{OwnerKind, PortReservation, ReservationOwner};
    use forge_core::store::{
        InstanceRepo, InstanceRow, NewToolVersion, PortRepo, Site, SiteKind, SiteRepo, Store,
        ToolVersionRepo, VersionSource, VersionState,
    };

    let php = online::extracted(&online::PHP_8_4);
    let nginx = online::extracted(&online::NGINX_1_30);
    let root = TestRoot::new();
    let nginx_dir = root.root.bin().join("nginx").join("1.30.6");
    online::copy_tree(
        &php,
        &root.root.bin().join("php").join("8.4.27-nts-vs17-x64"),
    );
    online::copy_tree(&nginx, &nginx_dir);
    for dir in [
        "logs/nginx",
        "tmp/nginx",
        "logs/php/8.4",
        "tmp/php/8.4/sessions",
        "tmp/php/8.4/uploads",
    ] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();
    let http = free_port();
    {
        std::fs::create_dir_all(root.root.state()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        for name in ["loja", "blog"] {
            let app = root.root.www().join(name);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(
                app.join("index.php"),
                "<?php echo 'site ', $_SERVER['FORGEBOX_SITE'] ?? '-', ' php ', PHP_VERSION, ' pid ', getmypid();\n",
            )
            .unwrap();
            let host = format!("{name}.test");
            let mut site = Site::new(&host, host.replace('.', "-"));
            site.kind = SiteKind::Php;
            site.root = Some(forge_core::paths::posix_string(&app));
            site.driver = Some("generic-php".into());
            site.php.series = Some("8.4".into());
            site.secure = false;
            site.http_redirect = false;
            SiteRepo::new(store.conn()).insert(&site).unwrap();
        }
        let nginx_tool = forge_core::ids::ToolId::new("nginx").unwrap();
        let version = ToolVersionRepo::new(store.conn())
            .register_if_absent(&NewToolVersion {
                tool_id: nginx_tool.clone(),
                tool_name: "Nginx".into(),
                version: "1.30.6".into(),
                canonical_name: "1.30.6".into(),
                variant: BTreeMap::new(),
                path: forge_core::paths::posix_string(&nginx_dir),
                state: VersionState::Installed,
                source: VersionSource::Manual,
            })
            .unwrap()
            .unwrap();
        let mut instance = InstanceRow::new(nginx_tool, version, "default");
        instance.autostart = true;
        InstanceRepo::new(store.conn()).insert(&instance).unwrap();
        for (name, port) in [("http", http), ("https", free_port())] {
            let owner =
                ReservationOwner::new(OwnerKind::Instance, instance.id.to_string(), name).unwrap();
            PortRepo::new(store.conn())
                .insert(&PortReservation::new_tcp(port, Some("web".into()), owner))
                .unwrap();
        }
        store.close().unwrap();
    }

    let before_cgi = pids_of("php-cgi.exe");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(90)).await;
    let deadline = Instant::now() + Duration::from_secs(90);
    while !http_get(http, "loja.test")
        .await
        .contains("site loja.test php 8.4.27")
    {
        assert!(
            Instant::now() < deadline,
            "loja.test not served:\n{}",
            daemon.output()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let stop_load = Arc::new(AtomicBool::new(false));
    let clients: Vec<_> = [
        "loja.test",
        "loja.test",
        "loja.test",
        "loja.test",
        "blog.test",
    ]
    .into_iter()
    .map(|host| {
        let stop = Arc::clone(&stop_load);
        tokio::spawn(async move {
            let (mut ok, mut failed, mut retried) = (0u32, Vec::new(), 0u32);
            while !stop.load(Ordering::Relaxed) {
                let mut answer = http_get(http, host).await;
                if is_reload_drop(&answer) {
                    // Closed before any byte by nginx's reload: retried once, like a browser.
                    retried += 1;
                    answer = http_get(http, host).await;
                }
                let wanted = format!("site {host} php 8.4.27");
                if answer.starts_with("HTTP/1.1 200") && answer.contains(&wanted) {
                    ok += 1;
                } else {
                    failed.push(answer.lines().next().unwrap_or_default().to_owned());
                }
            }
            (host, ok, failed, retried)
        })
    })
    .collect();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = Instant::now();
    let loja = pool_mode(&client, "loja.test", SitePoolMode::Dedicated).await;
    println!("dedicated in {:?}", started.elapsed());
    assert_eq!(upstream_of(&loja).as_deref(), Some("php_8_4__loja-test"));
    let dedicated = wait_pools(&client, "the dedicated pool runs", |p| {
        p.iter()
            .any(|p| p.upstream == "php_8_4__loja-test" && p.state == ServiceState::Running)
    })
    .await
    .into_iter()
    .find(|p| p.upstream == "php_8_4__loja-test")
    .unwrap();
    assert_eq!(dedicated.sites, 1, "{dedicated:?}");
    assert!(!dedicated.ports.is_empty(), "{dedicated:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let started = Instant::now();
    let loja = pool_mode(&client, "loja.test", SitePoolMode::Shared).await;
    println!("shared again in {:?}", started.elapsed());
    assert_eq!(upstream_of(&loja).as_deref(), Some("php_8_4"));
    wait_pools(&client, "the dedicated pool stops", |p| {
        p.iter().all(|p| p.upstream != "php_8_4__loja-test")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    stop_load.store(true, Ordering::Relaxed);
    let mut total = 0;
    for task in clients {
        let (host, ok, failed, retried) = task.await.unwrap();
        println!(
            "{host}: {ok} ok, {} failed {failed:?}, {retried} retried after a reload drop",
            failed.len()
        );
        assert!(failed.is_empty(), "{host}: {failed:?}");
        total += ok;
    }
    assert!(total >= 200, "only {total} requests");

    // The dedicated pool drains in the background; then its workers' ports go back.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let ports = client.call::<services_ports>(NoParams {}).await.unwrap();
        let held: Vec<u16> = ports
            .ports
            .iter()
            .filter(|p| p.owner_id.contains('~'))
            .map(|p| p.port)
            .collect();
        if held.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ports of the dedicated pool still reserved: {held:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let pid = daemon.pid();
    let (exit, _) = stop(&client, &mut daemon, Duration::from_secs(90)).await;
    assert!(exit.success(), "{}", daemon.output());
    let left: Vec<u32> = pids_of("php-cgi.exe")
        .into_iter()
        .filter(|p| !before_cgi.contains(p))
        .collect();
    assert!(left.is_empty(), "php-cgi left running: {left:?}");
    assert!(children_of(&[pid]).is_empty());
}
