//! The web path of the boot (docs/01 §5.1 steps 7-8) with real binaries: nginx 1.30.6
//! and PHP 8.4.27 in `bin/`, a PHP site and an nginx instance already in the store (as
//! after a restart; its web ports reserved on free high ports). The boot starts the
//! site's pool, renders `etc/nginx` (main config, upstream `php_8_4`, the site) and starts
//! nginx (its `nginx -t` pre-check passes); an HTTP request with `Host: app.test` runs the
//! site's PHP through the pool. Then `sites.php app.test 8.3` switches the site live to
//! PHP 8.3.36 while requests keep coming (none may fail) and the 8.4 pool is drained.
//! Quick settings on the real nginx (M2-E2-T7): `services.set nginx/default` rewrites the
//! directives inside the managed block of `nginx.conf` (the `http {}` ones in place) and
//! the reload / restart (each validated by `nginx -t`) keeps serving.
//! `daemon.stop` leaves neither nginx nor php-cgi running.
//!
//! `cargo test -p forge-daemon --test web_nginx_php_online -- --ignored --nocapture`.

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use forge_core::ids::ToolId;
use forge_core::model::port::{OwnerKind, PortReservation, ReservationOwner};
use forge_core::store::{
    InstanceRepo, InstanceRow, NewToolVersion, PortRepo, Site, SiteKind, SiteRepo, Store,
    ToolVersionRepo, VersionSource, VersionState,
};
use forge_ipc::api::methods::{
    jobs_get, php_pool_status, services_set, services_status, sites_list, sites_php,
};
use forge_ipc::api::{
    JobIdParams, JobState, NoParams, PhpPoolStatusParams, ServiceSetParams, ServiceState,
    SitePhpParams, SiteRef,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::*;

async fn http_get(port: u16, host: &str, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the official nginx 1.30.6 and PHP 8.4.27 zips (shared cache) and VC++ 2022"]
async fn boot_renders_and_starts_nginx_in_front_of_the_php_pool_online() {
    let php = online::extracted(&online::PHP_8_4);
    let php83 = online::extracted(&online::PHP_8_3);
    let nginx = online::extracted(&online::NGINX_1_30);
    let root = TestRoot::new();
    let nginx_dir = root.root.bin().join("nginx").join("1.30.6");
    online::copy_tree(
        &php,
        &root.root.bin().join("php").join("8.4.27-nts-vs17-x64"),
    );
    online::copy_tree(
        &php83,
        &root.root.bin().join("php").join("8.3.36-nts-vs16-x64"),
    );
    online::copy_tree(&nginx, &nginx_dir);
    // What `post_install` of the manifests creates on a real installation.
    for dir in [
        "logs/nginx",
        "tmp/nginx",
        "logs/php/8.4",
        "tmp/php/8.4/sessions",
        "tmp/php/8.4/uploads",
        "logs/php/8.3",
        "tmp/php/8.3/sessions",
        "tmp/php/8.3/uploads",
    ] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    let app = root.root.www().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("index.php"),
        "<?php echo 'site ', getenv('FORGEBOX_SITE') ?: $_SERVER['FORGEBOX_SITE'] ?? '-', ' php ', PHP_VERSION;\n",
    )
    .unwrap();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();

    let (http, https) = (free_port(), free_port());
    {
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
        let nginx_tool = ToolId::new("nginx").unwrap();
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
        for (name, port) in [("http", http), ("https", https)] {
            let owner =
                ReservationOwner::new(OwnerKind::Instance, instance.id.to_string(), name).unwrap();
            PortRepo::new(store.conn())
                .insert(&PortReservation::new_tcp(port, Some("web".into()), owner))
                .unwrap();
        }
        store.close().unwrap();
    }

    let before_cgi = pids_of("php-cgi.exe");
    let before_nginx = pids_of("nginx.exe");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, ready) = connect(&root, &mut daemon, Duration::from_secs(90)).await;
    println!("api after {ready:?}");
    let started = Instant::now();
    let status = loop {
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        let nginx_up = status
            .instances
            .iter()
            .any(|i| i.tool.as_str() == "nginx" && i.state == ServiceState::Running);
        let pool_up = status
            .pools
            .iter()
            .any(|p| p.state == ServiceState::Running);
        if nginx_up && pool_up {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "not running: {status:#?}\n{}",
            daemon.output()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let nginx_instance = status
        .instances
        .iter()
        .find(|i| i.tool.as_str() == "nginx")
        .unwrap();
    println!(
        "nginx pid {:?} ports {:?}; pool {} ports {:?}",
        nginx_instance.pid,
        nginx_instance
            .ports
            .iter()
            .map(|p| (p.name.clone(), p.port))
            .collect::<Vec<_>>(),
        status.pools[0].upstream,
        status.pools[0].ports
    );

    // Rendered configuration.
    let etc = root.root.etc().join("nginx");
    let upstream = std::fs::read_to_string(etc.join("upstreams").join("php_8_4.conf")).unwrap();
    for port in &status.pools[0].ports {
        assert!(
            upstream.contains(&format!("127.0.0.1:{port}")),
            "{upstream}"
        );
    }
    let site_conf = std::fs::read_to_string(etc.join("sites").join("app.test.conf")).unwrap();
    assert!(
        site_conf.contains("server_name app.test *.app.test"),
        "{site_conf}"
    );
    assert!(!site_conf.contains('\r'));
    let main = std::fs::read_to_string(etc.join("nginx.conf")).unwrap();
    assert!(
        main.contains(&format!("listen 127.0.0.1:{http} default_server")),
        "{main}"
    );
    assert!(!main.contains('~'), "long paths only:\n{main}");

    // A request through nginx reaches PHP with FORGEBOX_SITE.
    let answer = http_get(http, "app.test", "/").await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains("site app.test php 8.4.27"), "{answer}");
    let unknown = http_get(http, "nope.test", "/").await;
    assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
    let access = std::fs::read_to_string(root.root.logs().join("nginx").join("access.json"))
        .unwrap_or_default();
    assert!(
        access.contains("\"host\":\"app.test\""),
        "JSON access log: {access}"
    );
    let sites = client.call::<sites_list>(NoParams {}).await.unwrap();
    assert_eq!(
        sites.sites[0].php.as_ref().unwrap().resolved.as_deref(),
        Some("8.4.27")
    );

    // Quick settings on the real nginx: `reload` keys (`nginx -s reload`), then a
    // `restart` key (`worker_processes`, validated by `nginx -t` before the stop).
    for (pairs, wanted) in [
        (
            vec![("client_max_body_size", "1G"), ("keepalive_timeout", "30")],
            vec!["    client_max_body_size 1G;", "    keepalive_timeout 30;"],
        ),
        (vec![("worker_processes", "2")], vec!["worker_processes 2;"]),
    ] {
        let job = client
            .call::<services_set>(ServiceSetParams {
                target: "nginx/default".into(),
                settings: pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            })
            .await
            .unwrap();
        let started = Instant::now();
        loop {
            let job = client
                .call::<jobs_get>(JobIdParams { job_id: job.id })
                .await
                .unwrap();
            if job.state == JobState::Done {
                break;
            }
            assert!(
                !job.state.is_terminal(),
                "services.set {pairs:?}: {:?}",
                job.error
            );
            assert!(started.elapsed() < Duration::from_secs(60));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let main = std::fs::read_to_string(etc.join("nginx.conf")).unwrap();
        for line in wanted {
            assert!(main.lines().any(|l| l == line), "{line:?} in:\n{main}");
        }
        assert_eq!(
            main.matches("client_max_body_size").count(),
            1,
            "one directive, never a duplicate:\n{main}"
        );
        let answer = http_get(http, "app.test", "/").await;
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }

    // Live switch of the site's PHP (docs/06 §2.2) while requests keep coming: the 8.3
    // pool starts, nginx is re-rendered and reloaded, the 8.4 pool is drained.
    let load = tokio::spawn(async move {
        let (mut ok, mut bad) = (0u32, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let answer = http_get(http, "app.test", "/").await;
            if answer.starts_with("HTTP/1.1 200") {
                ok += 1;
            } else {
                bad.push(answer.lines().next().unwrap_or_default().to_owned());
            }
        }
        (ok, bad)
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let job = client
        .call::<sites_php>(SitePhpParams {
            site: SiteRef::from("app.test"),
            php: "8.3".into(),
        })
        .await
        .unwrap();
    let switch_started = Instant::now();
    loop {
        let info = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        match info.state {
            JobState::Done => break,
            JobState::Failed | JobState::Cancelled => panic!("{:?}", info.error),
            _ => {}
        }
        assert!(switch_started.elapsed() < Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("sites.php 8.3 done in {:?}", switch_started.elapsed());
    let (ok, bad) = load.await.unwrap();
    println!(
        "requests during the switch: {ok} ok, {} failed {bad:?}",
        bad.len()
    );
    assert!(bad.is_empty(), "requests failed during the switch: {bad:?}");
    let answer = http_get(http, "app.test", "/").await;
    assert!(answer.contains("site app.test php 8.3.36"), "{answer}");
    let drained = Instant::now();
    loop {
        let pools = client
            .call::<php_pool_status>(PhpPoolStatusParams { version: None })
            .await
            .unwrap();
        if pools.pools.iter().all(|p| p.version == "8.3.36") {
            break;
        }
        assert!(drained.elapsed() < Duration::from_secs(60), "{pools:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!(
        "8.4 pool out of service {:?} after the switch (drains in the background)",
        drained.elapsed()
    );

    let new_cgi: Vec<u32> = pids_of("php-cgi.exe")
        .into_iter()
        .filter(|p| !before_cgi.contains(p))
        .collect();
    let new_nginx: Vec<u32> = pids_of("nginx.exe")
        .into_iter()
        .filter(|p| !before_nginx.contains(p))
        .collect();
    assert!(!new_cgi.is_empty() && !new_nginx.is_empty());
    let pid = daemon.pid();
    let (exit, took) = stop(&client, &mut daemon, Duration::from_secs(90)).await;
    client.close();
    assert!(exit.success(), "{}", daemon.output());
    println!("stopped in {took:?}");
    let left: Vec<u32> = pids_of("php-cgi.exe")
        .into_iter()
        .chain(pids_of("nginx.exe"))
        .filter(|p| new_cgi.contains(p) || new_nginx.contains(p))
        .collect();
    assert!(left.is_empty(), "left running: {left:?}");
    assert!(children_of(&[pid]).is_empty());
    show_log(&root);
}
