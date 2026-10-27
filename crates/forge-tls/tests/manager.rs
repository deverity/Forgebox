//! The `tls` actor: issuance, owner-only keys, events, renewal (manual and scheduled),
//! trust and rotation.

mod support;

use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::actor::ActorSupervisor;
use forge_core::event::{Event, EventBus, EventKind, EventStream};
use forge_core::ids::SiteId;
use forge_core::platform::{FileAcl, TrustScope};
use forge_ipc::api::events::TlsPayload;
use forge_tls::ca::{CA_CERT_FILE, CA_KEY_FILE, CaOptions, LocalCa};
use forge_tls::leaf::{LEAF_VALIDITY, LeafRequest, issue_leaf, load_leaf};
use forge_tls::trust::{LEDGER_FILE, TrustLedger};
use forge_tls::{RenewScope, TlsConfig, TlsManager};
use jiff::SignedDuration;
use pretty_assertions::assert_eq;
use support::{FakeTrust, TempRoot, temp_root};

struct Fixture {
    root: TempRoot,
    tls: TlsManager,
    trust: Arc<FakeTrust>,
    events: EventStream,
    acl: Arc<dyn FileAcl>,
}

fn config() -> TlsConfig {
    let mut config = TlsConfig::new(vec![".test".to_owned()]);
    config.ca_common_name = Some("Forgebox Local CA manager-test 2026".to_owned());
    config
}

fn spawn_on(root: TempRoot, config: TlsConfig) -> Fixture {
    let bus = EventBus::new();
    let events = bus.subscribe();
    let trust = FakeTrust::new();
    // The real owner-only ACL of this OS (DACL on Windows).
    let acl = forge_core::platform::current().shared_file_acl();
    let tls = TlsManager::spawn(
        &ActorSupervisor::new(bus),
        &root.root,
        config,
        Arc::clone(&acl),
        trust.clone(),
    );
    Fixture {
        root,
        tls,
        trust,
        events,
        acl,
    }
}

fn fixture() -> Fixture {
    spawn_on(temp_root(), config())
}

async fn next_event(events: &mut EventStream) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event in time")
        .expect("bus open")
}

/// The next `tls.*` event (skips `actor.*` and `job.*`).
async fn next_tls(events: &mut EventStream) -> (EventKind, TlsPayload) {
    loop {
        let event = next_event(events).await;
        if matches!(
            event.kind,
            EventKind::TlsCaCreated | EventKind::TlsLeafIssued | EventKind::TlsRenewed
        ) {
            // Contract: the producer's payload is the API's TlsPayload.
            let payload: TlsPayload = serde_json::from_value(event.payload.clone())
                .unwrap_or_else(|e| panic!("{}: {e}: {}", event.kind, event.payload));
            assert_eq!(payload.v, 1);
            return (event.kind, payload);
        }
    }
}

fn no_more_tls(events: &mut EventStream) {
    while let Some(event) = events.try_recv() {
        assert!(
            !matches!(
                event.kind,
                EventKind::TlsCaCreated | EventKind::TlsLeafIssued | EventKind::TlsRenewed
            ),
            "unexpected {}",
            event.kind
        );
    }
}

#[tokio::test]
async fn issue_creates_the_ca_and_a_leaf_with_owner_only_keys() {
    let mut f = fixture();
    let site = SiteId::new();
    let leaf = f
        .tls
        .issue(LeafRequest::new("loja.test").with_site(site))
        .await
        .unwrap();
    assert_eq!(leaf.sans, ["loja.test", "*.loja.test"]);
    let lifetime = leaf.not_after.duration_since(leaf.not_before);
    assert_eq!(lifetime, LEAF_VALIDITY);

    let (kind, ca_event) = next_tls(&mut f.events).await;
    assert_eq!(kind, EventKind::TlsCaCreated);
    assert_eq!(ca_event.host, None);
    let (kind, leaf_event) = next_tls(&mut f.events).await;
    assert_eq!(kind, EventKind::TlsLeafIssued);
    assert_eq!(leaf_event.host.as_deref(), Some("loja.test"));
    assert_eq!(leaf_event.site_id, Some(site));
    assert_eq!(leaf_event.thumbprint_sha256, leaf.thumbprint_sha256);
    assert_eq!(leaf_event.not_after, leaf.not_after);

    let ca_dir = f.root.root.certs_ca();
    let sites = f.root.root.certs_sites();
    for key in [ca_dir.join(CA_KEY_FILE), sites.join("loja.test.key")] {
        assert!(key.exists(), "{}", key.display());
        assert!(
            f.acl.is_restricted_to_current_user(&key).unwrap(),
            "{} must grant access to the current user only",
            key.display()
        );
    }
    for public in [ca_dir.join(CA_CERT_FILE), sites.join("loja.test.crt")] {
        assert!(
            !f.acl.is_restricted_to_current_user(&public).unwrap(),
            "{} keeps the inherited ACL",
            public.display()
        );
        let text = std::fs::read_to_string(&public).unwrap();
        assert!(text.starts_with("-----BEGIN CERTIFICATE-----\n") && !text.contains('\r'));
    }
    let leftovers: Vec<_> = std::fs::read_dir(&sites)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Same request: the existing certificate, nothing new.
    let again = f.tls.issue(LeafRequest::new("loja.test")).await.unwrap();
    assert_eq!(again.thumbprint_sha256, leaf.thumbprint_sha256);
    tokio::time::sleep(Duration::from_millis(100)).await;
    no_more_tls(&mut f.events);

    // More names: a new certificate.
    let wider = f
        .tls
        .issue(LeafRequest::new("loja.test").with_aliases(["api.loja.test"]))
        .await
        .unwrap();
    assert_ne!(wider.thumbprint_sha256, leaf.thumbprint_sha256);
    assert_eq!(wider.sans, ["loja.test", "*.loja.test", "api.loja.test"]);
    let (kind, _) = next_tls(&mut f.events).await;
    assert_eq!(kind, EventKind::TlsLeafIssued);

    let err = f
        .tls
        .issue(LeafRequest::new("google.com"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input", "{err}");

    let status = f.tls.status().await.unwrap();
    let ca = status.ca.unwrap();
    assert_eq!(
        ca.name_constraints(),
        [".test", ".localhost", "127.0.0.0/8", "::1/128"]
    );
    assert!(!status.trusted_current_user && !status.trusted_local_machine);
    assert!(!status.ca_expires_soon && status.missing_tlds.is_empty());
    assert_eq!(status.leaves.len(), 1);
    assert!(status.leaves[0].signed_by_ca && status.leaves[0].key_ok && !status.leaves[0].due);
    assert!(status.warnings.is_empty(), "{:?}", status.warnings);

    let pem = f.tls.export_ca().await.unwrap();
    assert_eq!(
        pem,
        std::fs::read_to_string(ca_dir.join(CA_CERT_FILE)).unwrap()
    );
    let export = f.tls.export_leaf("loja.test").await.unwrap();
    assert!(export.key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
    f.tls.shutdown().await;
}

/// Writes a leaf for `host` that expires in `days`, signed by the CA on disk.
fn short_leaf(root: &TempRoot, acl: &dyn FileAcl, host: &str, days: i64) {
    let ca = LocalCa::load(&root.root.certs_ca()).unwrap().unwrap();
    let (host, sans) = LeafRequest::new(host).sans().unwrap();
    let leaf = issue_leaf(
        &ca,
        &root.root.certs_sites(),
        &host,
        &sans,
        SignedDuration::from_hours(days * 24),
        Timestamp::now(),
    )
    .unwrap();
    leaf.save(acl).unwrap();
}

#[tokio::test]
async fn leaves_with_less_than_30_days_are_renewed() {
    let mut f = fixture();
    f.tls.ensure_ca().await.unwrap();
    f.tls.issue(LeafRequest::new("blog.test")).await.unwrap();
    short_leaf(&f.root, f.acl.as_ref(), "loja.test", 10);
    f.tls.reload().await.unwrap();

    let status = f.tls.status().await.unwrap();
    let due: Vec<_> = status
        .leaves
        .iter()
        .map(|l| (l.info.host.as_str(), l.due))
        .collect();
    assert_eq!(due, [("blog.test", false), ("loja.test", true)]);

    while next_tls(&mut f.events).await.0 != EventKind::TlsLeafIssued {}
    let renewed = f.tls.renew(RenewScope::Due).await.unwrap();
    assert_eq!(renewed.len(), 1);
    assert_eq!(renewed[0].host, "loja.test");
    let left = renewed[0].not_after.duration_since(Timestamp::now());
    assert!(left > SignedDuration::from_hours(824 * 24), "{left:?}");
    let (kind, payload) = next_tls(&mut f.events).await;
    assert_eq!(kind, EventKind::TlsRenewed);
    assert_eq!(payload.host.as_deref(), Some("loja.test"));

    assert!(f.tls.renew(RenewScope::Due).await.unwrap().is_empty());
    let forced = f
        .tls
        .renew(RenewScope::Host("blog.test".to_owned()))
        .await
        .unwrap();
    assert_eq!(forced.len(), 1);
    let err = f
        .tls
        .renew(RenewScope::Host("nope.test".to_owned()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert_eq!(f.tls.renew(RenewScope::All).await.unwrap().len(), 2);
    f.tls.shutdown().await;
}

#[tokio::test]
async fn the_scheduler_renews_due_leaves_on_its_own() {
    // CA and a 5-day leaf on disk before the manager starts.
    let root = temp_root();
    let acl = forge_core::platform::current().shared_file_acl();
    let ca = LocalCa::generate(&CaOptions::for_this_machine(
        vec![".test".to_owned()],
        Timestamp::now(),
    ))
    .unwrap();
    ca.save(&root.root.certs_ca(), acl.as_ref()).unwrap();
    short_leaf(&root, acl.as_ref(), "loja.test", 5);
    let before = load_leaf(&root.root.certs_sites(), "loja.test")
        .unwrap()
        .unwrap();

    let mut config = config();
    config.first_check_delay = Duration::from_millis(50);
    config.check_interval = Duration::from_millis(200);
    let mut f = spawn_on(root, config);
    let (kind, payload) = next_tls(&mut f.events).await;
    assert_eq!(kind, EventKind::TlsRenewed);
    assert_eq!(payload.host.as_deref(), Some("loja.test"));
    let after = load_leaf(&f.root.root.certs_sites(), "loja.test")
        .unwrap()
        .unwrap();
    assert_ne!(after.info.thumbprint_sha256, before.info.thumbprint_sha256);
    assert_eq!(after.info.thumbprint_sha256, payload.thumbprint_sha256);
    assert!(after.signed_by(ca.cert_der()));
    // Later checks find nothing due.
    tokio::time::sleep(Duration::from_millis(600)).await;
    no_more_tls(&mut f.events);
    f.tls.shutdown().await;
}

#[tokio::test]
async fn install_and_uninstall_keep_the_ledger() {
    let f = fixture();
    let report = f.tls.install_ca(TrustScope::CurrentUser).await.unwrap();
    assert!(report.added);
    let thumbprint = report.ca.thumbprint_sha1;
    assert!(f.trust.contains(thumbprint, TrustScope::CurrentUser));
    let again = f.tls.install_ca(TrustScope::CurrentUser).await.unwrap();
    assert!(!again.added, "already trusted: no second dialog");
    assert_eq!(f.trust.calls().len(), 1);

    let ca_dir = f.root.root.certs_ca();
    let ledger = TrustLedger::load(&ca_dir).unwrap();
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.entries[0].thumbprint_sha1, thumbprint);
    assert!(ca_dir.join(LEDGER_FILE).exists());
    let status = f.tls.status().await.unwrap();
    assert!(status.trusted_current_user && !status.trusted_local_machine);

    let removed = f.tls.uninstall_ca(TrustScope::CurrentUser).await.unwrap();
    assert_eq!(removed, [thumbprint]);
    assert!(TrustLedger::load(&ca_dir).unwrap().entries.is_empty());
    assert!(!f.tls.status().await.unwrap().trusted_current_user);
    f.tls.shutdown().await;

    // Portable mode never touches the OS store.
    let mut portable = config();
    portable.os_trust = false;
    let p = spawn_on(temp_root(), portable);
    let err = p.tls.install_ca(TrustScope::CurrentUser).await.unwrap_err();
    assert_eq!(err.code(), "forbidden");
    assert!(p.trust.calls().is_empty());
    assert!(!p.tls.status().await.unwrap().warnings.is_empty());
    p.tls.shutdown().await;
}

#[tokio::test]
async fn rotation_trusts_the_new_ca_first_and_reissues_every_leaf() {
    let mut f = fixture();
    let old = f.tls.install_ca(TrustScope::CurrentUser).await.unwrap().ca;
    f.tls.issue(LeafRequest::new("loja.test")).await.unwrap();
    f.tls
        .issue(LeafRequest::new("blog.test").with_aliases(["api.blog.test"]))
        .await
        .unwrap();
    for _ in 0..3 {
        next_tls(&mut f.events).await;
    }

    let report = f.tls.rotate_ca().await.unwrap();
    assert_ne!(report.ca.thumbprint_sha1, old.thumbprint_sha1);
    assert_eq!(
        report.previous.as_ref().unwrap().thumbprint_sha1,
        old.thumbprint_sha1
    );
    assert_eq!(report.trusted, [TrustScope::CurrentUser]);
    assert_eq!(report.removed_previous, [TrustScope::CurrentUser]);
    assert!(
        report.warnings.is_empty() && report.skipped.is_empty(),
        "{report:?}"
    );
    let hosts: Vec<_> = report.leaves.iter().map(|l| l.host.as_str()).collect();
    assert_eq!(hosts, ["blog.test", "loja.test"]);

    // New root added before the old one was removed.
    let calls = f.trust.calls();
    assert_eq!(
        calls[1..],
        [
            format!("add current_user {}", report.ca.thumbprint_sha1),
            format!("remove current_user {}", old.thumbprint_sha1),
        ]
    );
    assert!(
        f.trust
            .contains(report.ca.thumbprint_sha1, TrustScope::CurrentUser)
    );
    assert!(
        !f.trust
            .contains(old.thumbprint_sha1, TrustScope::CurrentUser)
    );

    let new_ca = LocalCa::load(&f.root.root.certs_ca()).unwrap().unwrap();
    assert_eq!(new_ca.info().thumbprint_sha1, report.ca.thumbprint_sha1);
    for host in ["loja.test", "blog.test"] {
        let leaf = load_leaf(&f.root.root.certs_sites(), host)
            .unwrap()
            .unwrap();
        assert!(leaf.signed_by(new_ca.cert_der()), "{host}");
        assert!(leaf.key_ok);
    }
    let blog = load_leaf(&f.root.root.certs_sites(), "blog.test")
        .unwrap()
        .unwrap();
    assert_eq!(
        blog.info.sans,
        ["blog.test", "*.blog.test", "api.blog.test"]
    );

    let ledger = TrustLedger::load(&f.root.root.certs_ca()).unwrap();
    let recorded: Vec<_> = ledger.entries.iter().map(|e| e.thumbprint_sha1).collect();
    assert_eq!(recorded, [report.ca.thumbprint_sha1]);

    let (kind, ca_event) = next_tls(&mut f.events).await;
    assert_eq!(kind, EventKind::TlsCaCreated);
    assert_eq!(ca_event.thumbprint_sha256, report.ca.thumbprint_sha256);
    for _ in 0..2 {
        assert_eq!(next_tls(&mut f.events).await.0, EventKind::TlsRenewed);
    }
    let status = f.tls.status().await.unwrap();
    assert!(status.stale_roots.is_empty());
    assert!(status.leaves.iter().all(|l| l.signed_by_ca && !l.due));
    f.tls.shutdown().await;
}

#[tokio::test]
async fn a_declined_dialog_leaves_the_old_ca_in_place() {
    let f = fixture();
    let old = f.tls.install_ca(TrustScope::CurrentUser).await.unwrap().ca;
    let leaf = f.tls.issue(LeafRequest::new("loja.test")).await.unwrap();
    *f.trust.decline.lock().unwrap() = true;

    let err = f.tls.rotate_ca().await.unwrap_err();
    assert_eq!(err.code(), "cancelled", "{err}");
    let on_disk = LocalCa::load(&f.root.root.certs_ca()).unwrap().unwrap();
    assert_eq!(on_disk.info().thumbprint_sha1, old.thumbprint_sha1);
    let still = load_leaf(&f.root.root.certs_sites(), "loja.test")
        .unwrap()
        .unwrap();
    assert_eq!(still.info.thumbprint_sha256, leaf.thumbprint_sha256);
    let status = f.tls.status().await.unwrap();
    assert_eq!(status.ca.unwrap().thumbprint_sha1, old.thumbprint_sha1);
    assert!(status.trusted_current_user);

    // Accepted the second time.
    *f.trust.decline.lock().unwrap() = false;
    let report = f.tls.rotate_ca().await.unwrap();
    assert_ne!(report.ca.thumbprint_sha1, old.thumbprint_sha1);
    f.tls.shutdown().await;
}

#[tokio::test]
async fn unusable_ca_files_are_replaced_by_a_rotation() {
    let root = temp_root();
    let acl = forge_core::platform::current().shared_file_acl();
    let opts = CaOptions::for_this_machine(vec![".test".to_owned()], Timestamp::now());
    let a = LocalCa::generate(&opts).unwrap();
    let b = LocalCa::generate(&opts).unwrap();
    a.save(&root.root.certs_ca(), acl.as_ref()).unwrap();
    // The key of another CA next to the certificate.
    std::fs::write(root.root.certs_ca().join(CA_KEY_FILE), b.key_pem()).unwrap();

    let f = spawn_on(root, config());
    let status = f.tls.status().await.unwrap();
    assert!(status.ca.is_none());
    assert!(
        status
            .ca_error
            .as_deref()
            .unwrap()
            .contains("does not match"),
        "{status:?}"
    );
    let err = f
        .tls
        .issue(LeafRequest::new("loja.test"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "config_invalid");

    let report = f.tls.rotate_ca().await.unwrap();
    assert!(report.previous.is_none());
    assert!(f.tls.status().await.unwrap().ca_error.is_none());
    f.tls.issue(LeafRequest::new("loja.test")).await.unwrap();
    f.tls.shutdown().await;
}

#[test]
fn an_interrupted_ca_write_is_finished_on_load() {
    let root = temp_root();
    let dir = root.root.certs_ca();
    let acl = support::RecordingAcl::default();
    let opts = CaOptions::for_this_machine(vec![".test".to_owned()], Timestamp::now());
    let old = LocalCa::generate(&opts).unwrap();
    old.save(&dir, &acl).unwrap();
    let new = LocalCa::generate(&opts).unwrap();
    // Crash after the key rename, before the certificate rename.
    std::fs::write(dir.join(CA_KEY_FILE), new.key_pem()).unwrap();
    std::fs::write(dir.join(format!("{CA_CERT_FILE}.tmp")), new.cert_pem()).unwrap();
    let loaded = LocalCa::load(&dir).unwrap().unwrap();
    assert_eq!(loaded.info().thumbprint_sha1, new.info().thumbprint_sha1);
    assert!(!dir.join(format!("{CA_CERT_FILE}.tmp")).exists());

    // Crash before the key rename: the old pair stays, the stray key goes away.
    std::fs::write(dir.join(format!("{CA_KEY_FILE}.tmp")), old.key_pem()).unwrap();
    let loaded = LocalCa::load(&dir).unwrap().unwrap();
    assert_eq!(loaded.info().thumbprint_sha1, new.info().thumbprint_sha1);
    assert!(!dir.join(format!("{CA_KEY_FILE}.tmp")).exists());
}
