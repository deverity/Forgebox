//! Parked directories: scan, reconcile and the real `notify` watcher (tempdir).

use std::path::Path;
use std::time::Duration;

use forge_core::event::{EventBus, EventKind};
use forge_core::store::Store;
use forge_sites::parked::{self, ParkedConfig, ParkedDir, ParkedSync, ParkedWatcher};
use forge_sites::{SiteDraft, SiteRules, SiteService};

fn service(store: &Store, bus: &EventBus) -> SiteService {
    SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        bus.clone(),
        SiteRules::default(),
    )
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "<?php\n").unwrap();
}

fn config(dir: &Path) -> ParkedConfig {
    ParkedConfig::new(vec![ParkedDir::new(dir)])
}

async fn hosts(svc: &SiteService) -> Vec<String> {
    svc.list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.host)
        .collect()
}

/// Polls `check` every 50 ms until it holds or `timeout` passes.
async fn eventually<F, Fut>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn scan_lists_project_folders_only() {
    let tmp = tempfile::tempdir().unwrap();
    let www = tmp.path();
    touch(&www.join("Loja/artisan"));
    touch(&www.join("Loja/public/index.php"));
    touch(&www.join("blog/index.php"));
    std::fs::create_dir_all(www.join("empty")).unwrap();
    std::fs::create_dir_all(www.join(".git")).unwrap();
    std::fs::create_dir_all(www.join("_old")).unwrap();
    std::fs::create_dir_all(www.join("Minha Loja_2")).unwrap();
    std::fs::write(www.join("index.php"), "<?php\n").unwrap();

    let entries = parked::scan(www, "test").unwrap();
    let found: Vec<_> = entries
        .iter()
        .map(|e| (e.folder.as_str(), e.host.as_str(), e.driver))
        .collect();
    assert_eq!(
        found,
        [
            ("Loja", "loja.test", "laravel"),
            ("Minha Loja_2", "minha-loja-2.test", "generic-php"),
            ("blog", "blog.test", "generic-php"),
            ("empty", "empty.test", "generic-php"),
        ]
    );
    assert!(parked::scan(&www.join("missing"), "test").is_err());
    assert!(!parked::is_parkable(".git") && !parked::is_parkable("_x") && parked::is_parkable("x"));
    assert!(!parked::is_parkable("loja.cs-tmp") && !parked::is_parkable("loja.cs-old"));
}

#[tokio::test]
async fn reconcile_adds_removes_and_refreshes_sites() {
    let tmp = tempfile::tempdir().unwrap();
    let www = tmp.path();
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = service(&store, &bus);
    let sync = ParkedSync::new(svc.clone(), config(www));
    let mut events = bus.subscribe();

    touch(&www.join("a/index.php"));
    std::fs::create_dir_all(www.join("b")).unwrap();
    let report = sync.reconcile().await.unwrap();
    assert_eq!(report.added, ["a.test", "b.test"]);
    assert_eq!(hosts(&svc).await, ["a.test", "b.test"]);
    let a = svc.get_by_host("a.test").await.unwrap().unwrap();
    assert!(a.parked);
    assert_eq!(a.driver.as_deref(), Some("generic-php"));
    assert_eq!(events.try_recv().unwrap().kind, EventKind::SiteCreated);
    assert_eq!(events.try_recv().unwrap().kind, EventKind::SiteCreated);

    // idempotent
    let again = sync.reconcile().await.unwrap();
    assert_eq!(again, parked::ReconcileReport::default());

    // `b` becomes a Laravel project: driver and root are refreshed
    touch(&www.join("b/artisan"));
    touch(&www.join("b/public/index.php"));
    let report = sync.reconcile().await.unwrap();
    assert_eq!(report.updated, ["b.test"]);
    let b = svc.get_by_host("b.test").await.unwrap().unwrap();
    assert_eq!(b.driver.as_deref(), Some("laravel"));
    assert!(b.root.unwrap().ends_with("/b/public"));

    // `a` disappears
    std::fs::remove_dir_all(www.join("a")).unwrap();
    let report = sync.reconcile().await.unwrap();
    assert_eq!(report.removed, ["a.test"]);
    assert_eq!(hosts(&svc).await, ["b.test"]);
}

#[tokio::test]
async fn explicit_site_wins_over_parked() {
    let tmp = tempfile::tempdir().unwrap();
    let www = tmp.path();
    let other = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = service(&store, &bus);
    let sync = ParkedSync::new(svc.clone(), config(www));

    // explicit first: the parked folder with the same host is skipped
    touch(&other.path().join("index.php"));
    let draft = SiteDraft::from_dir(other.path(), "loja.test", None).unwrap();
    svc.create(draft).await.unwrap();
    touch(&www.join("loja/index.php"));
    let report = sync.reconcile().await.unwrap();
    assert_eq!(report.skipped, ["loja.test"]);
    assert!(report.added.is_empty());
    let loja = svc.get_by_host("loja.test").await.unwrap().unwrap();
    assert!(!loja.parked);
    // and the folder disappearing does not touch the explicit site
    std::fs::remove_dir_all(www.join("loja")).unwrap();
    assert!(sync.reconcile().await.unwrap().removed.is_empty());
    assert_eq!(hosts(&svc).await, ["loja.test"]);

    // parked first: creating the explicit site replaces the implicit one
    touch(&www.join("shop/index.php"));
    sync.reconcile().await.unwrap();
    assert!(svc.get_by_host("shop.test").await.unwrap().unwrap().parked);
    let mut events = bus.subscribe();
    let draft = SiteDraft::from_dir(other.path(), "shop.test", None).unwrap();
    let created = svc.create(draft).await.unwrap();
    assert_eq!(created.replaced_parked.len(), 1);
    assert!(!svc.get_by_host("shop.test").await.unwrap().unwrap().parked);
    assert_eq!(events.try_recv().unwrap().kind, EventKind::SiteRemoved);
    assert_eq!(events.try_recv().unwrap().kind, EventKind::SiteCreated);
    // and explicit vs explicit is still a conflict
    let draft = SiteDraft::from_dir(other.path(), "shop.test", None).unwrap();
    assert!(svc.create(draft).await.is_err());
}

#[tokio::test]
async fn unreadable_parked_dir_keeps_its_sites() {
    let tmp = tempfile::tempdir().unwrap();
    let www = tmp.path().join("www");
    touch(&www.join("a/index.php"));
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = service(&store, &bus);
    let sync = ParkedSync::new(svc.clone(), config(&www));
    sync.reconcile().await.unwrap();
    std::fs::remove_dir_all(&www).unwrap();
    let report = sync.reconcile().await.unwrap();
    assert_eq!(report.unreadable, [www]);
    assert_eq!(hosts(&svc).await, ["a.test"]);
}

#[tokio::test]
async fn per_directory_tld_is_used() {
    let tmp = tempfile::tempdir().unwrap();
    touch(&tmp.path().join("app/index.php"));
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        bus.clone(),
        SiteRules::new(["test", "dev"]),
    );
    let mut cfg = config(tmp.path());
    cfg.dirs[0].tld = Some(".DEV".to_owned());
    ParkedSync::new(svc.clone(), cfg).reconcile().await.unwrap();
    assert_eq!(hosts(&svc).await, ["app.dev"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_creates_and_removes_sites_with_real_fs_events() {
    let tmp = tempfile::tempdir().unwrap();
    let www = tmp.path();
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = service(&store, &bus);
    touch(&www.join("existing/index.php"));
    let mut cfg = config(www);
    cfg.debounce = Duration::from_millis(500);

    let watcher = ParkedWatcher::start(ParkedSync::new(svc.clone(), cfg)).unwrap();
    let wait = Duration::from_secs(20);

    // the first reconcile picks up what is already there
    assert!(
        eventually(wait, || async { hosts(&svc).await == ["existing.test"] }).await,
        "initial reconcile"
    );

    // folder created -> site appears (hidden and _ folders are ignored)
    std::fs::create_dir_all(www.join(".hidden")).unwrap();
    std::fs::create_dir_all(www.join("_skip")).unwrap();
    std::fs::create_dir_all(www.join("Nova")).unwrap();
    assert!(
        eventually(wait, || async {
            hosts(&svc).await == ["existing.test", "nova.test"]
        })
        .await,
        "site created after mkdir, got {:?}",
        hosts(&svc).await
    );

    // folder deleted -> site disappears
    std::fs::remove_dir_all(www.join("Nova")).unwrap();
    assert!(
        eventually(wait, || async { hosts(&svc).await == ["existing.test"] }).await,
        "site removed after rmdir, got {:?}",
        hosts(&svc).await
    );

    watcher.stop().await;
    // stopped: no more reconciles
    std::fs::create_dir_all(www.join("late")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(hosts(&svc).await, ["existing.test"]);
}
