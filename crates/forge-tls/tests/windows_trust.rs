//! The Windows side: CryptoAPI (what Schannel, `curl.exe`, Chrome and Edge use) honours the
//! CA name constraints, and the trust store calls work on a real certificate store.
//!
//! `current_user_root_dialog_manual` touches the real `CurrentUser\Root` and needs a
//! person to accept the two Windows dialogs (add, then remove):
//! `cargo test -p forge-tls --test windows_trust -- --ignored --nocapture`.
#![cfg(windows)]

mod support;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::actor::ActorSupervisor;
use forge_core::event::EventBus;
use forge_core::platform::windows::WindowsTrustStore;
use forge_core::platform::{CertThumbprint, TrustScope, TrustStore, trust_store};
use forge_tls::ca::{CA_VALIDITY, CaOptions, LocalCa};
use forge_tls::crl::CA_CRL_FILE;
use forge_tls::leaf::{LEAF_VALIDITY, LeafRequest, issue_leaf};
use forge_tls::{TlsConfig, TlsManager};
use support::{RecordingAcl, forge_leaf, temp_root};

fn ca_on_disk(dir: &Path, common_name: &str, tlds: &[&str]) -> LocalCa {
    let ca = LocalCa::generate(&CaOptions {
        common_name: common_name.to_owned(),
        tlds: tlds.iter().map(|t| (*t).to_owned()).collect(),
        validity: CA_VALIDITY,
        now: Timestamp::now(),
    })
    .unwrap();
    ca.save(dir, &RecordingAcl::default()).unwrap();
    ca
}

fn leaf_for(ca: &LocalCa, host: &str) -> Vec<u8> {
    let (host, sans) = LeafRequest::new(host).sans().unwrap();
    issue_leaf(
        ca,
        Path::new("unused"),
        &host,
        &sans,
        LEAF_VALIDITY,
        Timestamp::now(),
        None,
    )
    .unwrap()
    .cert_der()
    .to_vec()
}

#[test]
fn cryptoapi_accepts_the_site_and_refuses_forged_real_domains() {
    let root = temp_root();
    let dir = root.root.certs_ca();
    let ca = ca_on_disk(&dir, "Forgebox Local CA chain-test 2026", &[".test"]);
    let os = trust_store(None);
    let leaf = leaf_for(&ca, "loja.test");

    os.verify_server(
        std::slice::from_ref(&leaf),
        ca.cert_der(),
        "loja.test",
        false,
    )
    .expect("loja.test validates with the Forgebox CA as root");
    os.verify_server(
        std::slice::from_ref(&leaf),
        ca.cert_der(),
        "api.loja.test",
        false,
    )
    .expect("*.loja.test");

    let err = os
        .verify_server(
            std::slice::from_ref(&leaf),
            ca.cert_der(),
            "google.com",
            false,
        )
        .unwrap_err();
    assert_eq!(err.code(), "verification_failed");
    assert!(err.to_string().contains("CERT_E_CN_NO_MATCH"), "{err}");

    // Control: a forged leaf inside the constraints passes, so below only the
    // constraints make the difference.
    let (forged, _) = forge_leaf(&dir, &["forged.test"], &[]);
    os.verify_server(&[forged], ca.cert_der(), "forged.test", false)
        .expect("inside the constraints");

    for evil in ["google.com", "evil.example.com"] {
        let (forged, _) = forge_leaf(&dir, &[evil], &[]);
        let err = os
            .verify_server(&[forged], ca.cert_der(), evil, false)
            .unwrap_err();
        println!("{evil}: {err}");
        assert_eq!(err.code(), "verification_failed", "{evil}: {err}");
        assert!(
            err.to_string().contains("NAME_CONSTRAINT"),
            "{evil}: refused for the constraints, not for something else: {err}"
        );
    }

    // Another CA as the only root: untrusted.
    let other_dir = root.dir.path().join("other");
    let other = ca_on_disk(&other_dir, "Forgebox Local CA other 2026", &[".test"]);
    let err = os
        .verify_server(&[leaf], other.cert_der(), "loja.test", false)
        .unwrap_err();
    assert_eq!(err.code(), "verification_failed", "{err}");
}

/// What Windows `curl.exe` does by default: check the leaf's revocation. With the CRL
/// responder of the `tls` actor the check passes; a leaf without a distribution point
/// (mkcert style) fails with "revocation status unknown".
#[tokio::test(flavor = "multi_thread")]
async fn cryptoapi_revocation_check_goes_through_the_crl_responder() {
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let root = temp_root();
    let mut config = TlsConfig::new(vec![".test".to_owned()]);
    config.crl_listen = Some(listen);
    let tls = TlsManager::spawn(
        &ActorSupervisor::new(EventBus::new()),
        &root.root,
        config,
        Arc::new(RecordingAcl::default()),
        trust_store(None),
    );
    let leaf = tls.issue(LeafRequest::new("loja.test")).await.unwrap();
    let ca = LocalCa::load(&root.root.certs_ca()).unwrap().unwrap();
    let url = format!(
        "http://127.0.0.1:{port}/forgebox/{}.crl",
        ca.info().thumbprint_sha1
    );
    assert_eq!(leaf.crl_url.as_deref(), Some(url.as_str()));
    let status = tls.status().await.unwrap();
    assert!(status.crl.serving, "{:?}", status.crl);
    assert_eq!(status.crl.url.as_deref(), Some(url.as_str()));
    assert!(root.root.certs_ca().join(CA_CRL_FILE).exists());

    tls.verify_with_os("loja.test")
        .await
        .expect("CryptoAPI fetched the CRL from the responder");

    let (host, sans) = LeafRequest::new("blog.test").sans().unwrap();
    let bare = issue_leaf(
        &ca,
        Path::new("unused"),
        &host,
        &sans,
        LEAF_VALIDITY,
        Timestamp::now(),
        None,
    )
    .unwrap()
    .cert_der()
    .to_vec();
    let os = trust_store(None);
    let anchor = ca.cert_der().to_vec();
    let err = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || os.verify_server(&[bare], &anchor, "blog.test", true)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap_err();
    println!("leaf without a distribution point: {err}");
    assert!(
        err.to_string().contains("REVOCATION_STATUS_UNKNOWN"),
        "{err}"
    );
    tls.shutdown().await;
}

/// Deletes the scratch store even when an assertion fails.
struct ScratchStore(String);

impl Drop for ScratchStore {
    fn drop(&mut self) {
        WindowsTrustStore::delete_user_store(&self.0).unwrap();
    }
}

#[test]
fn trust_store_calls_on_a_scratch_user_store() {
    let name = format!(
        "Forgebox-Test-{}-{}",
        std::process::id(),
        Timestamp::now().as_nanosecond()
    );
    let _cleanup = ScratchStore(name.clone());
    let store = WindowsTrustStore::new(None).with_user_store(&name);
    let root = temp_root();
    let ca = ca_on_disk(
        &root.root.certs_ca(),
        "Forgebox scratch-store test",
        &[".test"],
    );
    let thumbprint = CertThumbprint::of_der(ca.cert_der());
    assert_eq!(thumbprint, ca.info().thumbprint_sha1);

    assert!(
        !store
            .is_trusted(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );
    assert!(
        !store
            .remove_root(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );
    store
        .add_root(ca.cert_der(), TrustScope::CurrentUser)
        .unwrap();
    assert!(
        store
            .is_trusted(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );
    store
        .add_root(ca.cert_der(), TrustScope::CurrentUser)
        .expect("already present is success");
    assert!(
        !store
            .is_trusted(&thumbprint, TrustScope::LocalMachine)
            .unwrap(),
        "nothing reached the machine store"
    );
    assert!(
        store
            .remove_root(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );
    assert!(
        !store
            .is_trusted(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );
    assert!(
        !store
            .remove_root(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );

    assert!(WindowsTrustStore::user_store_exists(&name).unwrap());
    WindowsTrustStore::delete_user_store(&name).unwrap();
    assert!(
        !WindowsTrustStore::user_store_exists(&name).unwrap(),
        "the scratch store is gone, not just emptied"
    );
}

/// Manual: adds a throwaway, name-constrained root (`.invalid`, `localhost`, loopback;
/// its key is never stored) to the real `CurrentUser\Root` and removes it again. Windows
/// shows a confirmation dialog for each step.
#[test]
#[ignore = "manual: shows two Windows confirmation dialogs that a person must accept"]
fn current_user_root_dialog_manual() {
    let ca = LocalCa::generate(&CaOptions {
        common_name: "Forgebox trust test (safe to delete)".to_owned(),
        tlds: vec![".invalid".to_owned()],
        validity: CA_VALIDITY,
        now: Timestamp::now(),
    })
    .unwrap();
    let thumbprint = ca.info().thumbprint_sha1;
    let store = trust_store(None);
    println!(
        "constraints: {:?}; thumbprint {thumbprint}",
        ca.info().name_constraints()
    );
    assert!(
        !store
            .is_trusted(&thumbprint, TrustScope::CurrentUser)
            .unwrap()
    );

    println!("adding to CurrentUser\\Root: accept the Windows dialog (Yes)...");
    let added = store.add_root(ca.cert_der(), TrustScope::CurrentUser);
    println!("add_root -> {added:?}");
    added.unwrap();
    let trusted = store
        .is_trusted(&thumbprint, TrustScope::CurrentUser)
        .unwrap();
    println!("is_trusted(CurrentUser) after add -> {trusted}");
    assert!(trusted);
    let machine = store
        .is_trusted(&thumbprint, TrustScope::LocalMachine)
        .unwrap();
    println!("is_trusted(LocalMachine) -> {machine}");
    assert!(!machine);

    println!("removing from CurrentUser\\Root: accept the Windows dialog (Yes)...");
    let removed = store.remove_root(&thumbprint, TrustScope::CurrentUser);
    println!("remove_root -> {removed:?}");
    assert!(removed.unwrap());
    let trusted = store
        .is_trusted(&thumbprint, TrustScope::CurrentUser)
        .unwrap();
    println!("is_trusted(CurrentUser) after remove -> {trusted}");
    assert!(!trusted);
}
