//! PHP extension toggles and Xdebug modes (M2-E2-T9, docs/03 §2.3) against the real
//! `forgeboxd` on a temporary root with the fake PHP of `tests/fixtures/fake_php.rs`
//! (`php.exe` + a FastCGI `php-cgi.exe`, so the pool really runs and recycles):
//!
//! * `php.ext_disable|enable 8.4 intl` (a `defaults_on` extension with its DLL in `ext/`):
//!   `php.ext_list` follows, `conf.d/10-forgebox.ini` loses and gets back
//!   `extension = intl`, the running pool recycles (`pool.worker.recycled{reason: reload}`);
//!   a non-default `xsl` on then off gives back the overlay byte for byte and stores
//!   nothing; a second identical call changes nothing and recycles nothing; a DLL that is
//!   not there is `not_found`, a bad name `invalid_input`.
//! * `php.xdebug 8.4 trigger` (Xdebug installed into 8.4.27 as T8 leaves it): writes the
//!   manifest's toggle line and mode values into `20-xdebug.ini`, reserves 9003 for
//!   `php@8.4/xdebug` (`services.ports`), `PhpVersionDto.xdebug == trigger`; with 9003
//!   held by a listener first, `port_in_use` with the pid and nothing written. Then
//!   `on --port <p>` moves the reservation (an IDE listening on it later is fine), `off`
//!   writes the disabled line and keeps it; each call twice changes nothing.
//! * Xdebug missing (8.3): the job starts the install with `job.progress`. Release URLs
//!   must be https and the client trusts only real roots, so offline the download goes to a
//!   local server that does not speak TLS (a trusted unsigned copy of the official
//!   registry points the 8.3 build at it) and fails the job with nothing written; the
//!   successful install inside the job is the online test.
//! * Online (ignored): real PHP 8.4.27 + real Xdebug 3.5.3 from xdebug.org installed by
//!   `php.xdebug on` itself; `php -m` with the series overlays (what the pool's workers
//!   read) lists Xdebug.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use forge_core::ids::{JobId, ToolId};
use forge_core::store::{
    NewToolVersion, Store, ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};
use forge_ipc::api::methods::{
    jobs_get, php_ext_disable, php_ext_enable, php_ext_list, php_list, php_xdebug, services_ports,
    services_start,
};
use forge_ipc::api::{
    EventFilter, EventsListParams, JobDto, JobIdParams, JobState, NoParams, PhpExtListDto,
    PhpExtParams, PhpVersionParams, PhpXdebugParams, PoolDto, ServiceTargetsParams, XdebugMode,
};
use forge_ipc::{ApiError, Client, ClientError};
use futures::StreamExt;
use sha2::Digest;

use common::*;

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

fn long(path: &Path) -> PathBuf {
    forge_core::platform::long_path(path)
}

fn api_error(err: ClientError) -> ApiError {
    match err {
        ClientError::Api(api) => api,
        other => panic!("not an API error: {other:?}"),
    }
}

async fn wait_job(client: &Client, id: JobId, timeout: Duration) -> JobDto {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return job,
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn events_of(client: &Client, kind: &str) -> Vec<serde_json::Value> {
    client
        .events_list(EventsListParams {
            after_seq: 0,
            limit: Some(5000),
            filter: EventFilter::kinds([kind]),
        })
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

async fn reloads(client: &Client) -> usize {
    events_of(client, "pool.worker.recycled")
        .await
        .iter()
        .filter(|p| p["reason"] == "reload")
        .count()
}

/// Waits until more than `before` workers were recycled for a reload.
async fn wait_reloads(client: &Client, before: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let now = reloads(client).await;
        if now > before {
            return now;
        }
        assert!(Instant::now() < deadline, "the pool was not recycled");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn ext(version: &str, name: &str) -> PhpExtParams {
    PhpExtParams {
        version: version.into(),
        extension: name.into(),
    }
}

async fn toggle(client: &Client, enable: bool, name: &str) -> PhpExtListDto {
    let job = if enable {
        client.call::<php_ext_enable>(ext("8.4", name)).await
    } else {
        client.call::<php_ext_disable>(ext("8.4", name)).await
    }
    .unwrap();
    let done = wait_job(client, job.id, Duration::from_secs(60)).await;
    serde_json::from_value(done.result.unwrap()).unwrap()
}

fn enabled(list: &PhpExtListDto, name: &str) -> bool {
    list.extensions
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} not listed: {list:?}"))
        .enabled
}

/// `bin/php/<folder>/` with the fake `php.exe`/`php-cgi.exe` and empty extension DLLs.
fn php_tree(root: &TestRoot, folder: &str, dlls: &[&str]) -> PathBuf {
    let dir = root.root.bin().join("php").join(folder);
    std::fs::create_dir_all(dir.join("ext")).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    for name in dlls {
        std::fs::write(dir.join(format!("ext/php_{name}.dll")), b"").unwrap();
    }
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extension_toggles_render_the_overlay_and_recycle_the_pool() {
    let root = TestRoot::new();
    php_tree(&root, "8.4.27", &["curl", "mbstring", "intl", "xsl"]);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let overlay = long(&root.root.etc()).join("php/8.4/conf.d/10-forgebox.ini");

    // The pool runs (fake php-cgi workers), so toggles have something to recycle.
    let job = client
        .call::<services_start>(ServiceTargetsParams {
            targets: vec!["php@8.4".into()],
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    let initial = std::fs::read_to_string(&overlay).unwrap();
    assert!(initial.contains("\nextension = intl\n"), "{initial}");
    assert!(!initial.contains("extension = xsl"), "{initial}");

    let list = client
        .call::<php_ext_list>(PhpVersionParams {
            version: "8.4".into(),
        })
        .await
        .unwrap();
    let names: Vec<&str> = list.extensions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["curl", "intl", "mbstring", "xsl"]);
    assert!(enabled(&list, "intl") && !enabled(&list, "xsl"));
    assert!(list.extensions.iter().all(|e| e.bundled && !e.zend));

    // intl (on by default): off, then on again; each change recycles the pool.
    let before = reloads(&client).await;
    let off = toggle(&client, false, "intl").await;
    assert!(!enabled(&off, "intl"), "{off:?}");
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(!text.contains("extension = intl"), "{text}");
    let after_off = wait_reloads(&client, before).await;
    let on = toggle(&client, true, "intl").await;
    assert!(enabled(&on, "intl"));
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(text.contains("\nextension = intl\n"), "{text}");
    assert_eq!(text, initial, "back to the default: the same overlay");
    wait_reloads(&client, after_off).await;

    // xsl (off by default): on and off gives back the overlay byte for byte.
    let on = toggle(&client, true, "xsl").await;
    assert!(enabled(&on, "xsl"));
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(text.contains("\nextension = xsl\n"), "{text}");
    let off = toggle(&client, false, "XSL").await;
    assert!(!enabled(&off, "xsl"));
    assert_eq!(std::fs::read_to_string(&overlay).unwrap(), initial);

    // Idempotent: the same call again writes nothing and recycles nothing.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let settled = reloads(&client).await;
    let again = toggle(&client, false, "xsl").await;
    assert_eq!(again, off);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        reloads(&client).await,
        settled,
        "nothing changed, no recycle"
    );
    assert_eq!(std::fs::read_to_string(&overlay).unwrap(), initial);

    // A DLL that is not there; a name that is not an extension name.
    let err = api_error(
        client
            .call::<php_ext_enable>(ext("8.4", "imagick"))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "not_found", "{err:?}");
    assert!(err.message.contains("php_imagick.dll"), "{}", err.message);
    let err = api_error(
        client
            .call::<php_ext_disable>(ext("8.4", "../intl"))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input", "{err:?}");
    let err = api_error(
        client
            .call::<php_ext_enable>(ext("7.4", "intl"))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "not_found", "{err:?}");

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

// ----------------------------------------------------------------------------- Xdebug

/// A TCP server on 127.0.0.1 that answers anything with plain HTTP: a TLS client fails
/// its handshake at once (no network). Counts the connections.
struct NotTls {
    port: u16,
    hits: std::sync::Arc<AtomicUsize>,
}

impl NotTls {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
            }
        });
        Self { port, hits }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The official registry copied to `dir` with Xdebug's 8.3 build moved to
/// `https://127.0.0.1:<port>` (release URLs must be https), unsigned: the store marks it
/// trusted (development only). Installing Xdebug into 8.3 then really starts the download
/// inside the job, and fails there.
fn local_registry(dir: &Path, port: u16) {
    copy_dir(&repo_registry(), dir);
    std::fs::remove_file(dir.join("index.yaml.minisig")).unwrap();
    let path = dir.join("tools/xdebug.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    let from = "url: https://xdebug.org/files/php_xdebug-3.5.3-8.3-";
    let to = format!("url: https://127.0.0.1:{port}/php_xdebug-3.5.3-8.3-");
    assert!(text.contains(from));
    let text = text.replace(from, &to);
    std::fs::write(&path, &text).unwrap();
    let manifest_sha = hex(&sha2::Sha256::digest(text.as_bytes()));
    let index = std::fs::read_to_string(dir.join("index.yaml")).unwrap();
    let at = index.find("- id: xdebug").unwrap();
    let line = at + index[at..].find("sha256: ").unwrap() + "sha256: ".len();
    let index = format!("{}{manifest_sha}{}", &index[..line], &index[line + 64..]);
    std::fs::write(dir.join("index.yaml"), index).unwrap();
}

/// The registry row of the official registry pointing at `dir`, trusted unsigned.
fn seed_registry(store: &Store, dir: &Path) {
    store
        .conn()
        .execute(
            "INSERT INTO registries (id, name, url, kind, official, enabled, priority, \
             trusted_unsigned, public_key, ttl_secs, created_at, updated_at) \
             VALUES ('reg-test', 'official', ?1, 'dir', 1, 1, 10, 1, NULL, 3600, 'now', 'now')",
            [forge_core::paths::posix_string(dir)],
        )
        .unwrap();
}

/// `bin/php/<folder>/` registered like an installed version (`8.4.27-nts-vs17-x64`).
fn php_version(root: &TestRoot, store: &Store, folder: &str) -> ToolVersionRow {
    let dir = php_tree(root, folder, &["curl", "mbstring"]);
    let mut parts = folder.split('-');
    let version = parts.next().unwrap().to_owned();
    let (ts, compiler, arch) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let repo = ToolVersionRepo::new(store.conn());
    let (id, _) = repo
        .record_install(
            &NewToolVersion {
                tool_id: ToolId::new("php").unwrap(),
                tool_name: "PHP".into(),
                version,
                canonical_name: folder.into(),
                variant: BTreeMap::from([
                    ("ts".to_owned(), ts.to_owned()),
                    ("compiler".to_owned(), compiler.to_owned()),
                    ("arch".to_owned(), arch.to_owned()),
                ]),
                path: forge_core::paths::posix_string(&dir),
                state: VersionState::Installed,
                source: VersionSource::Registry,
            },
            None,
            None,
        )
        .unwrap();
    repo.get(id).unwrap().unwrap()
}

/// What `Layout::install_extension` leaves for `target` (ADR-0043): the DLL, the disabled
/// overlay of its post_install and the linked row.
fn seed_xdebug(root: &TestRoot, store: &Store, target: &ToolVersionRow, series: &str) {
    let dll = PathBuf::from(&target.path).join("ext/php_xdebug.dll");
    std::fs::write(&dll, b"xdebug").unwrap();
    let overlay = root
        .root
        .etc()
        .join(format!("php/{series}/conf.d/20-xdebug.ini"));
    std::fs::create_dir_all(overlay.parent().unwrap()).unwrap();
    std::fs::write(&overlay, ";zend_extension = xdebug\n").unwrap();
    let variant = BTreeMap::from([
        ("php_series".to_owned(), series.to_owned()),
        ("ts".to_owned(), target.variant["ts"].clone()),
        ("compiler".to_owned(), target.variant["compiler"].clone()),
        ("target_tool".to_owned(), "php".to_owned()),
        ("target_id".to_owned(), target.id.to_string()),
        ("target_name".to_owned(), target.canonical_name.clone()),
    ]);
    ToolVersionRepo::new(store.conn())
        .record_install(
            &NewToolVersion {
                tool_id: ToolId::new("xdebug").unwrap(),
                tool_name: "Xdebug".into(),
                version: "3.5.3".into(),
                canonical_name: format!("php-{}", target.canonical_name),
                variant,
                path: forge_core::paths::posix_string(&dll),
                state: VersionState::Installed,
                source: VersionSource::Registry,
            },
            None,
            None,
        )
        .unwrap();
}

async fn xdebug_job(
    client: &Client,
    version: &str,
    mode: XdebugMode,
    port: Option<u16>,
) -> Result<JobDto, ApiError> {
    client
        .call::<php_xdebug>(PhpXdebugParams {
            version: version.into(),
            mode,
            port,
        })
        .await
        .map_err(api_error)
}

async fn xdebug_call(
    client: &Client,
    mode: XdebugMode,
    port: Option<u16>,
) -> Result<(JobDto, PoolDto), ApiError> {
    let job = xdebug_job(client, "8.4", mode, port).await?;
    let done = wait_job(client, job.id, Duration::from_secs(60)).await;
    let pool = serde_json::from_value(done.result.clone().unwrap()).unwrap();
    Ok((done, pool))
}

async fn version_mode(client: &Client, series: &str) -> XdebugMode {
    client
        .call::<php_list>(NoParams {})
        .await
        .unwrap()
        .versions
        .into_iter()
        .find(|v| v.series == series)
        .unwrap()
        .xdebug
}

async fn xdebug_reservation(client: &Client, owner: &str) -> Option<u16> {
    client
        .call::<services_ports>(NoParams {})
        .await
        .unwrap()
        .ports
        .into_iter()
        .find(|p| p.owner_name == owner && p.name == "xdebug")
        .map(|p| p.port)
}

/// Whether 127.0.0.1:`port` can be listened on now.
fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xdebug_modes_reserve_the_client_port_and_install_inside_the_job() {
    let server = NotTls::start();
    let registry = tempfile::Builder::new().prefix("fbreg-").tempdir().unwrap();
    local_registry(registry.path(), server.port);
    let root = TestRoot::new();
    let (php84, php83) = {
        std::fs::create_dir_all(root.root.db().parent().unwrap()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        seed_registry(&store, registry.path());
        let php84 = php_version(&root, &store, "8.4.27-nts-vs17-x64");
        let php83 = php_version(&root, &store, "8.3.36-nts-vs16-x64");
        seed_xdebug(&root, &store, &php84, "8.4");
        (php84, php83)
    };
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let etc = long(&root.root.etc());
    let overlay = etc.join("php/8.4/conf.d/20-xdebug.ini");
    let seeded = std::fs::read(&overlay).unwrap();
    assert_eq!(version_mode(&client, "8.4").await, XdebugMode::Off);

    // 9003 (the default) when this machine has it free; else an explicit free port.
    let (first_port, explicit) = if port_free(9003) {
        (9003, None)
    } else {
        let port = free_port();
        println!("9003 is busy on this machine; testing with --port {port}");
        (port, Some(port))
    };

    // Held by a listener: port_in_use with the pid, and nothing written.
    {
        let _held = TcpListener::bind(("127.0.0.1", first_port)).unwrap();
        let err = xdebug_call(&client, XdebugMode::Trigger, explicit)
            .await
            .unwrap_err();
        if err.code != "port_in_use" {
            show_log(&root);
        }
        assert_eq!(err.code, "port_in_use", "{err:?}");
        let details = err.details.clone().unwrap_or_default();
        assert_eq!(
            details["args"]["pid"],
            serde_json::json!(std::process::id()),
            "{err:?}"
        );
        assert_eq!(std::fs::read(&overlay).unwrap(), seeded, "nothing written");
        assert_eq!(xdebug_reservation(&client, "php@8.4/xdebug").await, None);
        assert_eq!(version_mode(&client, "8.4").await, XdebugMode::Off);
    }

    // trigger: the manifest's toggle line and mode values, the port reserved.
    let (_, pool) = xdebug_call(&client, XdebugMode::Trigger, explicit)
        .await
        .unwrap();
    assert!(pool.xdebug, "{pool:?}");
    assert_eq!(
        (pool.series.as_str(), pool.version.as_str()),
        ("8.4", "8.4.27")
    );
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(text.contains("\nzend_extension=php_xdebug.dll\n"), "{text}");
    assert!(
        text.contains("\nxdebug.mode = \"debug,develop\"\n"),
        "{text}"
    );
    assert!(
        text.contains("\nxdebug.start_with_request = \"trigger\"\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nxdebug.client_port = \"{first_port}\"\n")),
        "{text}"
    );
    assert!(text.contains("\nxdebug.idekey = \"FORGEBOX\"\n"), "{text}");
    assert_eq!(
        xdebug_reservation(&client, "php@8.4/xdebug").await,
        Some(first_port)
    );
    assert_eq!(version_mode(&client, "8.4").await, XdebugMode::Trigger);
    let list = client
        .call::<php_ext_list>(PhpVersionParams {
            version: "8.4".into(),
        })
        .await
        .unwrap();
    let found: Vec<_> = list
        .extensions
        .iter()
        .filter(|e| e.name == "xdebug")
        .collect();
    assert_eq!(
        found.len(),
        1,
        "the registry DLL is not listed as bundled too"
    );
    assert!(
        found[0].enabled && found[0].zend && !found[0].bundled,
        "{found:?}"
    );
    let base = std::fs::read_to_string(etc.join("php/8.4/conf.d/10-forgebox.ini")).unwrap();
    assert!(!base.contains("xdebug"), "{base}");

    // Idempotent: the same call again answers the same and writes nothing.
    let before = std::fs::read(&overlay).unwrap();
    let (_, again) = xdebug_call(&client, XdebugMode::Trigger, explicit)
        .await
        .unwrap();
    assert_eq!(again, pool);
    assert_eq!(std::fs::read(&overlay).unwrap(), before);

    // on --port: the preset of `on`, the reservation moved.
    let moved = free_port();
    xdebug_call(&client, XdebugMode::On, Some(moved))
        .await
        .unwrap();
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(
        text.contains("\nxdebug.start_with_request = \"yes\"\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nxdebug.client_port = \"{moved}\"\n")),
        "{text}"
    );
    assert_eq!(
        xdebug_reservation(&client, "php@8.4/xdebug").await,
        Some(moved)
    );
    assert_eq!(version_mode(&client, "8.4").await, XdebugMode::On);

    // An IDE listening on the port we hold is the expected state: no port_in_use.
    {
        let _ide = TcpListener::bind(("127.0.0.1", moved)).unwrap();
        xdebug_call(&client, XdebugMode::On, Some(moved))
            .await
            .unwrap();
    }

    // off: the disabled line, the reservation kept, the extension still installed.
    let (_, pool) = xdebug_call(&client, XdebugMode::Off, None).await.unwrap();
    assert!(!pool.xdebug);
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(
        text.contains("\n;zend_extension=php_xdebug.dll\n"),
        "{text}"
    );
    assert!(
        !text.contains("\nzend_extension") && !text.contains("xdebug.mode"),
        "{text}"
    );
    assert_eq!(
        xdebug_reservation(&client, "php@8.4/xdebug").await,
        Some(moved)
    );
    assert_eq!(version_mode(&client, "8.4").await, XdebugMode::Off);
    assert!(
        PathBuf::from(&php84.path)
            .join("ext/php_xdebug.dll")
            .exists()
    );

    // php.ext_enable drives the same choice (the stored preset is `on`).
    let job = client
        .call::<php_ext_enable>(ext("8.4", "xdebug"))
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(30)).await;
    assert_eq!(version_mode(&client, "8.4").await, XdebugMode::On);
    let text = std::fs::read_to_string(&overlay).unwrap();
    assert!(text.contains("\nzend_extension=php_xdebug.dll\n"), "{text}");

    // Xdebug missing from 8.3: the job installs it first, with progress, and the download
    // (from a server that does not speak TLS) failing fails the job with nothing written.
    let port83 = free_port();
    let job = xdebug_job(&client, "8.3", XdebugMode::Trigger, Some(port83))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let failed = loop {
        let now = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if now.state != JobState::Running {
            break now;
        }
        assert!(Instant::now() < deadline, "install job still running");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(failed.state, JobState::Failed, "{failed:?}");
    println!("install failure reported as {:?}", failed.error);
    assert!(server.hits() >= 1, "the download was attempted");
    // The job's progress, replayed from the start of the event stream.
    let mut sub = client
        .subscribe_events(EventFilter::kinds(["job.progress"]), Some(0))
        .await
        .unwrap();
    let mut progress: Vec<String> = Vec::new();
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_millis(500), sub.next()).await
    {
        let payload = envelope.event.payload;
        if payload["job_id"] == serde_json::json!(job.id) {
            if let Some(message) = payload["message"].as_str() {
                progress.push(message.to_owned());
            }
        }
    }
    println!("progress of the 8.3 install: {progress:?}");
    assert!(
        progress.iter().any(|m| m.contains("xdebug")),
        "no install progress: {progress:?}"
    );
    assert!(
        !PathBuf::from(&php83.path)
            .join("ext/php_xdebug.dll")
            .exists()
    );
    assert!(!etc.join("php/8.3/conf.d/20-xdebug.ini").exists());
    assert_eq!(xdebug_reservation(&client, "php@8.3/xdebug").await, None);
    assert_eq!(version_mode(&client, "8.3").await, XdebugMode::Off);

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

/// Real PHP 8.4.27 (the official zip from the cache, extracted to the root) and the real
/// Xdebug 3.5.3 from xdebug.org: after `php.xdebug 8.4 on`, `php -m` run with the series
/// overlays (what the pool's workers read) lists Xdebug.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads Xdebug 3.5.3 from xdebug.org and uses the real PHP 8.4.27"]
async fn xdebug_on_loads_the_real_extension_online() {
    let root = TestRoot::new();
    let php_dir = root.root.bin().join("php").join("8.4.27-nts-vs17-x64");
    common::online::copy_tree(
        &common::online::extracted(&common::online::PHP_8_4),
        &php_dir,
    );
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(120)).await;
    let port = free_port();
    let job = client
        .call::<php_xdebug>(PhpXdebugParams {
            version: "8.4".into(),
            mode: XdebugMode::On,
            port: Some(port),
        })
        .await
        .unwrap();
    let started = Instant::now();
    let done = wait_job(&client, job.id, Duration::from_secs(300)).await;
    println!(
        "php.xdebug on (download + install + render): {:?}",
        started.elapsed()
    );
    let pool: PoolDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert!(pool.xdebug);
    let ini_dir = long(&root.root.etc()).join("php/8.4");
    let output = std::process::Command::new(php_dir.join("php.exe"))
        .arg("-m")
        .env("PHPRC", &ini_dir)
        .env("PHP_INI_SCAN_DIR", ini_dir.join("conf.d"))
        .output()
        .unwrap();
    let modules = String::from_utf8_lossy(&output.stdout);
    let errors = String::from_utf8_lossy(&output.stderr);
    println!("{modules}\n{errors}");
    assert!(modules.contains("Xdebug"), "{modules}\n{errors}");
    assert!(!errors.contains("Failed loading"), "{errors}");
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}
