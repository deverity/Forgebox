//! The `site.*` payloads published by `SiteService` decode as the API contract
//! `forge_ipc::api::events::SitePayload` without losing any field (docs/03 §8).

use std::path::Path;

use forge_core::event::{Event, EventBus, EventKind};
use forge_core::store::Store;
use forge_ipc::api::events::SitePayload;
use forge_sites::{SiteDraft, SiteRules, SiteService};
use serde_json::Value;

fn draft(host: &str, parked: bool) -> SiteDraft {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/projects/laravel");
    let mut d = SiteDraft::from_dir(&dir, host, None).unwrap();
    d.parked = parked;
    d
}

/// Decodes `event` as `SitePayload`, checking that every field sent is kept.
fn decode(event: &Event, kind: EventKind) -> SitePayload {
    assert_eq!(event.kind, kind);
    let json: &Value = &event.payload;
    let api: SitePayload = serde_json::from_value(json.clone())
        .unwrap_or_else(|e| panic!("{kind} payload is not a SitePayload: {e}\n{json}"));
    let back = serde_json::to_value(&api).unwrap();
    for (field, value) in json.as_object().expect("payload is an object") {
        assert_eq!(back.get(field), Some(value), "{kind}: field `{field}` lost");
    }
    assert_eq!(api.v, 1);
    api
}

#[tokio::test]
async fn site_events_decode_as_site_payload() {
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let svc = SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        bus.clone(),
        SiteRules::default(),
    );
    let mut events = bus.subscribe();

    // a parked site that the explicit one replaces
    let parked = svc
        .create_parked_if_free(draft("loja.test", true))
        .await
        .unwrap()
        .unwrap();
    let p = decode(&events.try_recv().unwrap(), EventKind::SiteCreated);
    assert_eq!((p.site_id, p.host.as_str()), (parked.id, "loja.test"));

    let created = svc.create(draft("loja.test", false)).await.unwrap();
    assert_eq!(created.replaced_parked.len(), 1);
    let p = decode(&events.try_recv().unwrap(), EventKind::SiteRemoved);
    assert_eq!(p.site_id, parked.id);
    let p = decode(&events.try_recv().unwrap(), EventKind::SiteCreated);
    assert_eq!(p.site_id, created.site.id);

    let mut site = created.site;
    site.aliases.push("admin.loja.test".to_owned());
    let site = svc.update(site).await.unwrap();
    let p = decode(&events.try_recv().unwrap(), EventKind::SiteUpdated);
    assert_eq!((p.site_id, p.host.as_str()), (site.id, "loja.test"));

    svc.remove(site.id).await.unwrap();
    let p = decode(&events.try_recv().unwrap(), EventKind::SiteRemoved);
    assert_eq!(p.site_id, site.id);
}
