//! Boot with a PHP site already in the store and the official PHP 8.4.27 (NTS, VS17)
//! copied into `bin/php/` as a loose folder: the boot discovers the version (`php.exe`
//! answers), resolves the site to it and starts its FastCGI pool (docs/01 §5.1 steps
//! 3 and 7). A FastCGI request to a worker runs the site's `index.php`; `daemon.stop`
//! leaves no `php-cgi.exe` behind. Three rounds on the same root.
//!
//! Needs the PHP zip (downloaded once into `C:\apps_src\Forgebox-wt\cache`, or
//! `FORGEBOX_TEST_CACHE`, sha256 checked) and the VC++ runtime of the machine:
//! `cargo test -p forge-daemon --test php_pool_online -- --ignored --nocapture`.

mod common;

use std::time::{Duration, Instant};

use forge_core::store::{Site, SiteKind, SiteRepo, Store};
use forge_fcgi::probe::{FcgiRequest, request};
use forge_ipc::api::methods::{php_list, php_pool_status, services_status};
use forge_ipc::api::{NoParams, PhpPoolStatusParams, ServiceState};

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the official PHP 8.4.27 zip (downloaded into the shared cache) and VC++ 2022"]
async fn boot_starts_the_pool_of_a_configured_php_site_online() {
    let php = online::extracted(&online::PHP_8_4);
    let root = TestRoot::new();
    online::copy_tree(
        &php,
        &root.root.bin().join("php").join("8.4.27-nts-vs17-x64"),
    );
    let app = root.root.www().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("index.php"),
        "<?php echo 'ok ', PHP_VERSION, ' ', php_sapi_name();\n",
    )
    .unwrap();
    {
        // The site exists before the daemon runs (as after a restart).
        std::fs::create_dir_all(root.root.state()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        let mut site = Site::new("app.test", "app-test");
        site.kind = SiteKind::Php;
        site.root = Some(forge_core::paths::posix_string(&app));
        site.driver = Some("generic-php".into());
        site.php.series = Some("8.4".into());
        site.secure = false;
        site.http_redirect = false;
        SiteRepo::new(store.conn()).insert(&site).unwrap();
        store.close().unwrap();
    }
    // `www/app` would also be parked as `app.test`: keep only the stored site.
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();

    let before: Vec<u32> = pids_of("php-cgi.exe");
    for round in 0..3 {
        let mut daemon = spawn_daemon(&root, &[]);
        let (client, ready) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
        let started = Instant::now();
        let pool = loop {
            let pools = client
                .call::<php_pool_status>(PhpPoolStatusParams { version: None })
                .await
                .unwrap();
            if let Some(pool) = pools
                .pools
                .into_iter()
                .find(|p| p.state == ServiceState::Running && p.workers >= 2)
            {
                break pool;
            }
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "pool not running:\n{}",
                daemon.output()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        println!(
            "round {round}: api after {ready:?}, pool {} {} running {:?} later with ports {:?}",
            pool.upstream,
            pool.version,
            started.elapsed(),
            pool.ports
        );
        assert_eq!(pool.version, "8.4.27");
        assert_eq!(pool.series, "8.4");
        assert_eq!(pool.upstream, "php_8_4");
        assert_eq!(pool.sites, 1);
        let versions = client.call::<php_list>(NoParams {}).await.unwrap();
        assert_eq!(versions.versions.len(), 1, "discovered from bin/php");
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        assert_eq!(status.pools.len(), 1);

        let addr = format!("127.0.0.1:{}", pool.ports[0]).parse().unwrap();
        let script = app.join("index.php");
        let answer = request(
            addr,
            &FcgiRequest::get("/index.php", &script, &app),
            Duration::from_secs(10),
        )
        .await
        .expect("FastCGI request");
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body_text(), "ok 8.4.27 cgi-fcgi");

        let workers: Vec<u32> = pids_of("php-cgi.exe")
            .into_iter()
            .filter(|p| !before.contains(p))
            .collect();
        assert!(workers.len() >= 2, "{workers:?}");
        let pid = daemon.pid();
        let (exit, took) = stop(&client, &mut daemon, Duration::from_secs(90)).await;
        client.close();
        assert!(exit.success(), "{}", daemon.output());
        println!("round {round}: stopped in {took:?} (workers {workers:?})");
        let left: Vec<u32> = pids_of("php-cgi.exe")
            .into_iter()
            .filter(|p| workers.contains(p))
            .collect();
        assert!(left.is_empty(), "php-cgi left running: {left:?}");
        assert!(children_of(&[pid]).is_empty(), "{:?}", children_of(&[pid]));
        assert!(root.stray_lock_files().is_empty());
        assert!(!pipe_exists(&root.root).await);
    }
}
