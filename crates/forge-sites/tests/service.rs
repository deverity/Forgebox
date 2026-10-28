//! `SiteService`: host validation, uniqueness and `site.*` events.

use std::path::Path;

use forge_core::error::ForgeError;
use forge_core::event::{EventBus, EventKind};
use forge_core::store::Store;
use forge_sites::{SiteDraft, SiteRules, SiteService};

fn setup() -> (Store, EventBus, SiteService) {
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        bus.clone(),
        SiteRules::default(),
    );
    (store, bus, svc)
}

fn draft(host: &str) -> SiteDraft {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("projects")
        .join("laravel");
    SiteDraft::from_dir(&dir, host, None).unwrap()
}

#[tokio::test]
async fn crud_emits_events() {
    let (_store, bus, svc) = setup();
    let mut events = bus.subscribe();

    let created = svc.create(draft("loja.test")).await.unwrap();
    assert!(created.warnings.is_empty());
    let id = created.site.id;
    let event = events.try_recv().unwrap();
    assert_eq!(event.kind, EventKind::SiteCreated);
    assert_eq!(event.payload["host"], "loja.test");
    assert_eq!(event.payload["v"], 1);

    let mut site = svc.get(id).await.unwrap().unwrap();
    assert_eq!(site.driver.as_deref(), Some("laravel"));
    assert!(site.root.as_deref().unwrap().ends_with("/laravel/public"));
    site.aliases.push("Admin.Loja.test".to_owned());
    let site = svc.update(site).await.unwrap();
    assert_eq!(site.aliases, ["admin.loja.test"]);
    assert_eq!(events.try_recv().unwrap().kind, EventKind::SiteUpdated);

    svc.remove(id).await.unwrap();
    assert_eq!(events.try_recv().unwrap().kind, EventKind::SiteRemoved);
    assert!(svc.get(id).await.unwrap().is_none());
    assert!(matches!(svc.remove(id).await, Err(ForgeError::NotFound(_))));
    assert!(events.try_recv().is_none(), "failed remove emits nothing");
}

#[tokio::test]
async fn uppercase_host_is_normalized_with_a_warning() {
    let (_store, _bus, svc) = setup();
    let created = svc.create(draft("Loja.test")).await.unwrap();
    assert_eq!(created.site.host, "loja.test");
    assert_eq!(created.warnings.len(), 1);
    assert_eq!(created.warnings[0].key, "sites.host_lowercased");
    assert!(created.warnings[0].message.contains("Loja.test"));
    assert!(svc.get_by_host("LOJA.TEST").await.unwrap().is_some());
}

#[tokio::test]
async fn invalid_hosts_are_rejected_and_nothing_is_stored() {
    let (_store, bus, svc) = setup();
    let mut events = bus.subscribe();
    for bad in ["loja_x.test", "loja.com", "loja", "lo ja.test"] {
        let err = svc.create(draft(bad)).await.unwrap_err();
        assert!(matches!(err, ForgeError::InvalidInput(_)), "{bad}: {err}");
    }
    assert!(svc.list().await.unwrap().is_empty());
    assert!(events.try_recv().is_none());
}

#[tokio::test]
async fn hosts_and_aliases_are_unique_globally() {
    let (_store, _bus, svc) = setup();
    let mut a = draft("a.test");
    a.aliases = vec!["shared.test".to_owned()];
    svc.create(a).await.unwrap();

    let mut b = draft("b.test");
    b.aliases = vec!["shared.test".to_owned()];
    assert!(matches!(svc.create(b).await, Err(ForgeError::Conflict(_))));
    assert!(matches!(
        svc.create(draft("shared.test")).await,
        Err(ForgeError::Conflict(_))
    ));
    let mut c = draft("c.test");
    c.aliases = vec!["a.test".to_owned()];
    assert!(matches!(svc.create(c).await, Err(ForgeError::Conflict(_))));
    assert_eq!(svc.list().await.unwrap().len(), 1);
}
