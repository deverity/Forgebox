//! `DnsPolicyManager` over an in-memory OS policy whose changes go through the real
//! `HelperClient` (validation included) and a `FakeElevation` (no UAC): NRPT first and
//! verified through the OS resolver, `hosts` fallback (no port 53, Group Policy table,
//! failed probe), declined prompts, site sync, events and their IPC contract.

mod support;

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use forge_core::event::{EventBus, EventKind, EventStream};
use forge_core::helper::{FakeElevation, HelperOp, HelperResponse};
use forge_core::platform::NrptSource;
use forge_ipc::api::events::{DnsPolicyPayload, HelperOpPayload};
use forge_ipc::api::net::{DnsMode, DnsPolicyReason, DnsPolicyState};
use forge_net::policy::{
    DnsPolicyManager, EnableRequest, PROBE_PREFIX, PolicyReason, PolicyState, PolicyTimeouts,
};
use support::{MemoryPolicy, STOCK_HOSTS};

struct Fixture {
    fake: Arc<FakeElevation>,
    os: Arc<MemoryPolicy>,
    manager: DnsPolicyManager,
    events: EventStream,
}

fn timeouts() -> PolicyTimeouts {
    PolicyTimeouts {
        probe_attempt: Duration::from_secs(2),
        probe_window: Duration::from_millis(300),
        probe_interval: Duration::from_millis(50),
        ..PolicyTimeouts::default()
    }
}

fn fixture_os(fake: Arc<FakeElevation>, os: MemoryPolicy) -> Fixture {
    let os = Arc::new(os);
    let bus = EventBus::new();
    let events = bus.subscribe();
    let manager = DnsPolicyManager::new(os.clone())
        .with_events(bus)
        .with_timeouts(timeouts());
    Fixture {
        fake,
        os,
        manager,
        events,
    }
}

fn fixture_with(fake: FakeElevation, gpo: bool) -> Fixture {
    let fake = Arc::new(fake);
    let mut os = MemoryPolicy::new(fake.clone());
    if gpo {
        os = os.with_rule(NrptSource::GroupPolicy, ".corp.example", "10.0.0.1");
    }
    fixture_os(fake, os)
}

fn fixture() -> Fixture {
    fixture_with(FakeElevation::accepting(), false)
}

fn tlds() -> Vec<String> {
    vec![".test".into()]
}

fn loopback() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

fn drain(events: &mut EventStream) -> Vec<(EventKind, serde_json::Value)> {
    std::iter::from_fn(|| events.try_recv())
        .map(|e| (e.kind, e.payload))
        .collect()
}

fn last_applied(events: &[(EventKind, serde_json::Value)]) -> DnsPolicyPayload {
    let (_, payload) = events
        .iter()
        .rev()
        .find(|(kind, _)| *kind == EventKind::DnsPolicyApplied)
        .expect("dns.policy_applied");
    serde_json::from_value(payload.clone()).unwrap()
}

fn hosts_lines(os: &MemoryPolicy) -> Vec<String> {
    os.hosts_entries()
        .unwrap_or_default()
        .iter()
        .map(|e| format!("{} {}", e.ip, e.host))
        .collect()
}

fn enable_request(hosts: &[&str]) -> EnableRequest {
    EnableRequest {
        tlds: tlds(),
        nameserver: Some(loopback()),
        hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
    }
}

#[tokio::test]
async fn enable_adds_the_nrpt_rule_once_and_verifies_it() {
    let mut fx = fixture();
    let states = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(states.len(), 1);
    let state = &states[0];
    assert_eq!(
        (
            state.tld.as_str(),
            state.state,
            state.nameserver,
            state.reason
        ),
        (".test", PolicyState::Applied, Some(loopback()), None)
    );
    assert!(!state.group_policy_table);
    let probe = state.probe.as_ref().expect("verified");
    assert!(probe.ok, "{probe:?}");
    assert_eq!(probe.attempts, 1);
    assert!(probe.host.starts_with(PROBE_PREFIX) && probe.host.ends_with(".test"));
    assert_eq!(probe.resolved, vec![loopback()]);
    assert_eq!(
        fx.fake.ops(),
        vec![HelperOp::NrptAdd {
            namespace: ".test".into(),
            nameserver: loopback()
        }]
    );
    assert_eq!(fx.os.hosts_entries(), None, "hosts untouched");
    assert_eq!(
        fx.os.flushes.load(Ordering::Relaxed),
        1,
        "at the end; the first probe needs none"
    );
    assert!(fx.manager.hosts_mode_tlds().is_empty());

    // Events, deserialized with the IPC contract types.
    let events = drain(&mut fx.events);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].0, EventKind::HelperOp);
    let op: HelperOpPayload = serde_json::from_value(events[0].1.clone()).unwrap();
    assert_eq!(
        (op.v, op.name.as_str(), op.ok, op.error, op.effective),
        (1, "nrpt.add", true, None, Some(true))
    );
    assert_eq!(events[1].0, EventKind::DnsPolicyApplied);
    let applied: DnsPolicyPayload = serde_json::from_value(events[1].1.clone()).unwrap();
    assert_eq!(applied.v, 1);
    assert_eq!(applied.tld, ".test");
    assert_eq!(applied.policy, DnsPolicyState::Applied);
    assert_eq!(applied.mode, DnsMode::Nrpt);
    assert_eq!(applied.reason, None);
    assert_eq!(applied.probe_ok, Some(true));

    // Second time: already applied, no prompt (verified again).
    fx.manager.enable(enable_request(&[])).await.unwrap();
    assert_eq!(fx.fake.requests().len(), 1);

    // Status verifies with one probe.
    let status = fx.manager.status(&tlds(), Some(loopback())).await.unwrap();
    assert_eq!(status[0].state, PolicyState::Applied);
    assert!(status[0].probe.as_ref().unwrap().ok);

    // The resolver moved to 127.0.0.53: the rule is rewritten.
    let fallback: IpAddr = "127.0.0.53".parse().unwrap();
    let status = fx.manager.status(&tlds(), Some(fallback)).await.unwrap();
    assert_eq!(status[0].state, PolicyState::NotApplied, "points elsewhere");
    assert_eq!(status[0].nameserver, Some(loopback()));
    assert_eq!(status[0].reason, Some(PolicyReason::NrptPointsElsewhere));
    fx.manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: Some(fallback),
            hosts: vec![],
        })
        .await
        .unwrap();
    let status = fx.manager.status(&tlds(), Some(fallback)).await.unwrap();
    assert_eq!(status[0].state, PolicyState::Applied);
}

#[tokio::test]
async fn without_port_53_the_hosts_block_is_used() {
    let mut fx = fixture();
    let states = fx
        .manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: None,
            hosts: vec!["loja.test".into(), "api.loja.test".into(), "x.com".into()],
        })
        .await
        .unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert_eq!(states[0].reason, Some(PolicyReason::ResolverNotOnPort53));
    assert_eq!(states[0].probe, None, "nothing to verify");
    assert_eq!(
        hosts_lines(&fx.os),
        [
            "127.0.0.1 api.loja.test",
            "::1 api.loja.test",
            "127.0.0.1 loja.test",
            "::1 loja.test"
        ]
    );
    assert!(fx.os.hosts.lock().unwrap().starts_with(STOCK_HOSTS));
    let status = fx.manager.status(&tlds(), None).await.unwrap();
    assert_eq!(status[0].state, PolicyState::HostsFallback);
    let applied = last_applied(&drain(&mut fx.events));
    assert_eq!(applied.policy, DnsPolicyState::HostsFallback);
    assert_eq!(applied.mode, DnsMode::Hosts);
    assert_eq!(applied.reason, Some(DnsPolicyReason::ResolverNotOnPort53));

    // Sites change: one write; same sites again: no prompt.
    let sites = vec!["loja.test".into(), "blog.test".into()];
    assert!(fx.manager.sync_hosts(&sites, &tlds()).await.unwrap());
    assert!(!fx.manager.sync_hosts(&sites, &tlds()).await.unwrap());
    assert_eq!(fx.fake.requests().len(), 2);
    let hosts: Vec<String> = fx
        .os
        .hosts_entries()
        .unwrap()
        .into_iter()
        .map(|e| e.host)
        .collect();
    assert!(
        hosts.contains(&"blog.test".to_owned()) && !hosts.contains(&"api.loja.test".to_owned())
    );
}

#[tokio::test]
async fn an_empty_group_policy_table_skips_nrpt_and_uses_hosts() {
    // The M1 gate machine: the corporate GPO key exists with no rule; a local rule would
    // be ignored, so no prompt is spent on it (ADR-0033).
    let fake = Arc::new(FakeElevation::accepting());
    let os = MemoryPolicy::new(fake.clone()).with_group_policy_table();
    let mut fx = fixture_os(fake, os);
    let states = fx
        .manager
        .enable(enable_request(&["loja.test", "api.loja.test"]))
        .await
        .unwrap();
    let state = &states[0];
    assert_eq!(state.state, PolicyState::HostsFallback);
    assert_eq!(state.reason, Some(PolicyReason::GroupPolicyTablePresent));
    assert!(state.group_policy_table);
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect();
    assert_eq!(names, ["hosts.set_block"], "no nrpt.add");
    assert_eq!(
        hosts_lines(&fx.os),
        [
            "127.0.0.1 api.loja.test",
            "::1 api.loja.test",
            "127.0.0.1 loja.test",
            "::1 loja.test"
        ]
    );
    assert_eq!(fx.manager.hosts_mode_tlds(), tlds());
    let applied = last_applied(&drain(&mut fx.events));
    assert_eq!(applied.mode, DnsMode::Hosts);
    assert_eq!(
        applied.reason,
        Some(DnsPolicyReason::GroupPolicyTablePresent)
    );

    // Status: hosts mode with the reason, even with a stale local Forgebox rule.
    fx.os
        .rules
        .lock()
        .unwrap()
        .push(forge_core::platform::NrptRule {
            id: "{stale}".into(),
            namespaces: vec![".test".into()],
            nameservers: vec![loopback()],
            source: NrptSource::Local,
            comment: Some("Forgebox".into()),
        });
    let status = fx.manager.status(&tlds(), Some(loopback())).await.unwrap();
    assert_eq!(status[0].state, PolicyState::HostsFallback);
    assert_eq!(
        status[0].reason,
        Some(PolicyReason::GroupPolicyTablePresent)
    );
    assert_eq!(status[0].nameserver, Some(loopback()));
    assert_eq!(status[0].probe, None, "an ignored rule is not probed");

    // Disable removes the local rule AND the block.
    fx.manager.disable(&tlds()).await.unwrap();
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect();
    assert_eq!(names, ["hosts.set_block", "nrpt.remove", "hosts.set_block"]);
    assert_eq!(*fx.os.hosts.lock().unwrap(), STOCK_HOSTS, "byte for byte");
    assert!(fx.manager.hosts_mode_tlds().is_empty());
}

#[tokio::test]
async fn a_rule_the_helper_reports_ineffective_falls_back_to_hosts() {
    // The helper sees the Group Policy table (read elevated, or a GPO refresh between
    // the daemon's read and the write): `effective: false` is enough to go to hosts.
    let fake = Arc::new(FakeElevation::answering(|req| {
        let response = HelperResponse::success(req, "fake helper: done");
        Ok(match req.op {
            HelperOp::NrptAdd { .. } => response.with_details(serde_json::json!({
                "effective": false,
                "reason": "group_policy_table_present"
            })),
            _ => response,
        })
    }));
    let os = MemoryPolicy::new(fake.clone());
    let mut fx = fixture_os(fake, os);
    let states = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert_eq!(
        states[0].reason,
        Some(PolicyReason::GroupPolicyTablePresent)
    );
    assert_eq!(hosts_lines(&fx.os).len(), 2);
    let events = drain(&mut fx.events);
    let op: HelperOpPayload = serde_json::from_value(events[0].1.clone()).unwrap();
    assert_eq!(op.name, "nrpt.add");
    assert_eq!(op.effective, Some(false));
    assert_eq!(op.reason.as_deref(), Some("group_policy_table_present"));
}

#[tokio::test]
async fn a_failed_probe_falls_back_to_hosts() {
    // A VPN/security agent answers DNS itself: the rule is in the registry and looks
    // effective, but the OS resolver never reaches the Forgebox resolver.
    let fake = Arc::new(FakeElevation::accepting());
    let os = MemoryPolicy::new(fake.clone()).intercepting(Err("NXDOMAIN".into()));
    let mut fx = fixture_os(fake, os);
    let states = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    let state = &states[0];
    assert_eq!(state.state, PolicyState::HostsFallback);
    assert_eq!(state.reason, Some(PolicyReason::ResolverProbeFailed));
    assert_eq!(state.nameserver, Some(loopback()));
    let probe = state.probe.as_ref().unwrap();
    assert!(!probe.ok);
    assert!(probe.attempts >= 2, "retried within the window: {probe:?}");
    assert!(probe.error.as_deref().unwrap().contains("NXDOMAIN"));
    // Every try uses a fresh name; the cache is flushed between tries and at the end.
    let queries = fx.os.queries.lock().unwrap().clone();
    assert_eq!(queries.len(), probe.attempts as usize);
    let unique: std::collections::BTreeSet<_> = queries.iter().collect();
    assert_eq!(unique.len(), queries.len());
    assert_eq!(fx.os.flushes.load(Ordering::Relaxed), queries.len());

    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect();
    assert_eq!(names, ["nrpt.add", "hosts.set_block"]);
    assert_eq!(
        hosts_lines(&fx.os),
        ["127.0.0.1 loja.test", "::1 loja.test"]
    );
    let applied = last_applied(&drain(&mut fx.events));
    assert_eq!(applied.mode, DnsMode::Hosts);
    assert_eq!(applied.reason, Some(DnsPolicyReason::ResolverProbeFailed));
    assert_eq!(applied.probe_ok, Some(false));

    // Status reports the failed verification too.
    let status = fx.manager.status(&tlds(), Some(loopback())).await.unwrap();
    assert_eq!(status[0].state, PolicyState::HostsFallback);
    assert_eq!(status[0].reason, Some(PolicyReason::ResolverProbeFailed));
    assert!(!status[0].probe.as_ref().unwrap().ok);

    // An answer that is not loopback fails as well.
    *fx.os.intercept.lock().unwrap() = Some(Ok(vec!["10.1.2.3".parse().unwrap()]));
    let probe = fx.manager.probe(".test", Duration::ZERO).await;
    assert!(!probe.ok);
    assert_eq!(probe.attempts, 1);
    assert_eq!(probe.resolved, vec!["10.1.2.3".parse::<IpAddr>().unwrap()]);
}

#[tokio::test]
async fn group_policy_rules_force_the_hosts_fallback() {
    let fx = fixture_with(FakeElevation::accepting(), true);
    let states = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert_eq!(
        states[0].reason,
        Some(PolicyReason::GroupPolicyTablePresent)
    );
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect::<Vec<_>>();
    assert_eq!(names, ["hosts.set_block"]);
}

#[tokio::test]
async fn hosts_mode_follows_the_sites() {
    let fake = Arc::new(FakeElevation::accepting());
    let os = MemoryPolicy::new(fake.clone()).with_group_policy_table();
    let fx = fixture_os(fake, os);

    // Not in hosts mode yet: site changes never prompt.
    assert!(!fx.manager.sync_sites(&["a.test".into()]).await.unwrap());
    assert!(fx.fake.requests().is_empty());

    // No site yet: hosts mode is armed without writing anything.
    let states = fx.manager.enable(enable_request(&[])).await.unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert!(fx.fake.requests().is_empty());
    assert_eq!(fx.os.hosts_entries(), None);

    // Site and alias added: written; unchanged: no prompt.
    let sites: Vec<String> = vec!["loja.test".into(), "api.loja.test".into()];
    assert!(fx.manager.sync_sites(&sites).await.unwrap());
    assert!(!fx.manager.sync_sites(&sites).await.unwrap());
    assert_eq!(hosts_lines(&fx.os).len(), 4);
    // Alias removed.
    assert!(fx.manager.sync_sites(&sites[..1]).await.unwrap());
    assert_eq!(
        hosts_lines(&fx.os),
        ["127.0.0.1 loja.test", "::1 loja.test"]
    );
    // Last site removed: the block goes away, hosts mode stays armed.
    assert!(fx.manager.sync_sites(&[]).await.unwrap());
    assert_eq!(*fx.os.hosts.lock().unwrap(), STOCK_HOSTS);
    assert_eq!(fx.manager.hosts_mode_tlds(), tlds());
    assert!(fx.manager.sync_sites(&sites[..1]).await.unwrap());
    assert_eq!(hosts_lines(&fx.os).len(), 2);

    // A fresh manager without this root's state file does not take the block over
    // (it may be another root's): only the state file arms the mode.
    let again = DnsPolicyManager::new(fx.os.clone()).with_timeouts(timeouts());
    again.status(&tlds(), Some(loopback())).await.unwrap();
    assert!(again.hosts_mode_tlds().is_empty());
}

#[tokio::test]
async fn a_block_of_another_root_is_never_adopted() {
    // Review of ADR-0033: the `hosts` file is shared by every root of the machine. A
    // second root (portable, the smoke) boots with no state file while the block of the
    // first root is in `hosts`: boot (restore + status) and the first site sync must not
    // arm hosts mode, prompt, or touch that block.
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state").join("dns-policy.json");
    let fake = Arc::new(FakeElevation::accepting());
    let os = Arc::new(MemoryPolicy::new(fake.clone()).with_group_policy_table());
    let owner = DnsPolicyManager::new(os.clone()).with_timeouts(timeouts());
    owner.enable(enable_request(&["loja.test"])).await.unwrap();
    let block = os.hosts.lock().unwrap().clone();
    let before = fake.requests().len();

    let second = restarted(&os, &state).await;
    let status = second.status(&tlds(), Some(loopback())).await.unwrap();
    assert_eq!(status[0].state, PolicyState::NotApplied);
    assert_eq!(
        status[0].reason,
        Some(PolicyReason::GroupPolicyTablePresent)
    );
    assert!(second.hosts_mode_tlds().is_empty());
    assert!(!second.sync_sites(&["blog.test".into()]).await.unwrap());
    assert!(!second.sync_sites(&[]).await.unwrap());
    assert_eq!(fake.requests().len(), before, "no helper call");
    assert_eq!(*os.hosts.lock().unwrap(), block, "block untouched");
    assert!(!state.exists(), "nothing saved");
}

#[tokio::test]
async fn leaving_hosts_mode_for_a_verified_nrpt_removes_the_block() {
    // A VPN intercepted DNS at the first `enable` (hosts mode); once it is gone, `enable`
    // verifies NRPT and the now stale block goes away with it.
    let fake = Arc::new(FakeElevation::accepting());
    let os = MemoryPolicy::new(fake.clone()).intercepting(Err("NXDOMAIN".into()));
    let fx = fixture_os(fake, os);
    let states = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert_eq!(hosts_lines(&fx.os).len(), 2);

    *fx.os.intercept.lock().unwrap() = None;
    let states = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(states[0].state, PolicyState::Applied);
    assert!(fx.manager.hosts_mode_tlds().is_empty());
    assert_eq!(*fx.os.hosts.lock().unwrap(), STOCK_HOSTS);
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect();
    assert_eq!(names, ["nrpt.add", "hosts.set_block", "hosts.set_block"]);

    // Verified again: nothing left to remove, no prompt.
    fx.manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(fx.fake.requests().len(), 3);
}

#[tokio::test]
async fn a_declined_prompt_stops_everything() {
    let mut fx = fixture_with(FakeElevation::declining(), false);
    let err = fx
        .manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "needs_elevation");
    assert_eq!(
        fx.fake.requests().len(),
        1,
        "no hosts attempt after a refusal"
    );
    assert_eq!(fx.os.flushes.load(Ordering::Relaxed), 0);
    let events = drain(&mut fx.events);
    assert_eq!(events.len(), 1);
    let op: HelperOpPayload = serde_json::from_value(events[0].1.clone()).unwrap();
    assert!(!op.ok);
    assert!(op.error.unwrap().contains("declined"));
    assert_eq!(op.effective, None);
}

#[tokio::test]
async fn invalid_tlds_never_reach_the_helper() {
    let fx = fixture();
    let err = fx
        .manager
        .enable(EnableRequest {
            tlds: vec![".com".into()],
            nameserver: Some(loopback()),
            hosts: vec![],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");
    assert!(fx.fake.requests().is_empty());
}

#[tokio::test]
async fn disable_removes_rule_and_block() {
    let mut fx = fixture();
    fx.manager.enable(enable_request(&[])).await.unwrap();
    fx.manager
        .sync_hosts(&["loja.test".into()], &tlds())
        .await
        .unwrap();
    drain(&mut fx.events);
    fx.manager.disable(&tlds()).await.unwrap();
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "nrpt.add",
            "hosts.set_block",
            "nrpt.remove",
            "hosts.set_block"
        ]
    );
    assert_eq!(*fx.os.hosts.lock().unwrap(), STOCK_HOSTS, "byte for byte");
    assert!(fx.os.rules.lock().unwrap().is_empty());
    let status = fx.manager.status(&tlds(), None).await.unwrap();
    assert_eq!(status[0].state, PolicyState::NotApplied);
    let last = last_applied(&drain(&mut fx.events));
    assert_eq!(last.policy, DnsPolicyState::NotApplied);
    assert_eq!(last.mode, DnsMode::None);

    // Nothing left to remove: no prompt.
    let before = fx.fake.requests().len();
    fx.manager.disable(&tlds()).await.unwrap();
    assert_eq!(fx.fake.requests().len(), before);
}

/// A manager as the daemon builds it at boot: state file, then `restore`.
async fn restarted(os: &Arc<MemoryPolicy>, state: &std::path::Path) -> DnsPolicyManager {
    let manager = DnsPolicyManager::new(os.clone())
        .with_timeouts(timeouts())
        .with_state_file(state);
    manager.restore().await.unwrap();
    manager
}

#[tokio::test]
async fn hosts_mode_survives_a_restart_without_a_block() {
    // Review of ADR-0033: on the GPO machine `forge dns enable` ran before any site
    // existed (no block written), then the daemon restarted. The first site must still
    // reach hosts.
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state").join("dns-policy.json");
    let fake = Arc::new(FakeElevation::accepting());
    let os = Arc::new(MemoryPolicy::new(fake.clone()).with_group_policy_table());
    let first = DnsPolicyManager::new(os.clone())
        .with_timeouts(timeouts())
        .with_state_file(&state);
    let states = first.enable(enable_request(&[])).await.unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert!(fake.requests().is_empty(), "no site, no prompt");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({"v": 1, "hosts_mode": [".test"]}));

    // (a) Restart, nothing in hosts yet: status still says hosts, with the reason.
    let second = restarted(&os, &state).await;
    assert_eq!(second.hosts_mode_tlds(), tlds());
    let status = second.status(&tlds(), Some(loopback())).await.unwrap();
    assert_eq!(status[0].state, PolicyState::HostsFallback);
    assert_eq!(
        status[0].reason,
        Some(PolicyReason::GroupPolicyTablePresent)
    );
    assert!(second.sync_sites(&["loja.test".into()]).await.unwrap());
    assert_eq!(hosts_lines(&os), ["127.0.0.1 loja.test", "::1 loja.test"]);

    // (b) The last site goes away (block removed), restart, a new site arrives.
    assert!(second.sync_sites(&[]).await.unwrap());
    assert_eq!(*os.hosts.lock().unwrap(), STOCK_HOSTS);
    let third = restarted(&os, &state).await;
    assert!(third.sync_sites(&["blog.test".into()]).await.unwrap());
    assert_eq!(hosts_lines(&os), ["127.0.0.1 blog.test", "::1 blog.test"]);

    // Resolver not on port 53 after a restart: hosts mode, with that reason.
    let status = third.status(&tlds(), None).await.unwrap();
    assert_eq!(status[0].state, PolicyState::HostsFallback);
    assert_eq!(
        status[0].reason,
        Some(PolicyReason::GroupPolicyTablePresent),
        "the table explains it first"
    );

    // Disable clears the saved mode: after a restart, sites never prompt.
    third.disable(&tlds()).await.unwrap();
    assert_eq!(*os.hosts.lock().unwrap(), STOCK_HOSTS);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(saved["hosts_mode"], serde_json::json!([]));
    let fourth = restarted(&os, &state).await;
    assert!(fourth.hosts_mode_tlds().is_empty());
    let before = fake.requests().len();
    assert!(!fourth.sync_sites(&["loja.test".into()]).await.unwrap());
    assert_eq!(fake.requests().len(), before);
    let status = fourth.status(&tlds(), Some(loopback())).await.unwrap();
    assert_eq!(status[0].state, PolicyState::NotApplied);
}

#[tokio::test]
async fn hosts_mode_after_a_restart_without_port_53_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("dns-policy.json");
    let fx = fixture();
    let first = DnsPolicyManager::new(fx.os.clone())
        .with_timeouts(timeouts())
        .with_state_file(&state);
    first
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: None,
            hosts: vec![],
        })
        .await
        .unwrap();
    let again = restarted(&fx.os, &state).await;
    let status = again.status(&tlds(), None).await.unwrap();
    assert_eq!(status[0].state, PolicyState::HostsFallback);
    assert_eq!(status[0].reason, Some(PolicyReason::ResolverNotOnPort53));
}

#[tokio::test]
async fn a_missing_or_broken_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let os = Arc::new(MemoryPolicy::accepting());
    let none = DnsPolicyManager::new(os.clone()).with_state_file(dir.path().join("none.json"));
    assert!(none.restore().await.unwrap().is_empty());
    let broken = dir.path().join("broken.json");
    std::fs::write(&broken, b"{not json").unwrap();
    let manager = DnsPolicyManager::new(os).with_state_file(&broken);
    assert_eq!(manager.restore().await.unwrap_err().code(), "invalid_input");
    assert!(manager.hosts_mode_tlds().is_empty());
}

#[tokio::test]
async fn enable_again_in_hosts_mode_does_not_prompt() {
    let fake = Arc::new(FakeElevation::accepting());
    let os = MemoryPolicy::new(fake.clone()).with_group_policy_table();
    let fx = fixture_os(fake, os);
    let sites = ["loja.test", "api.loja.test"];
    fx.manager.enable(enable_request(&sites)).await.unwrap();
    assert_eq!(fx.fake.requests().len(), 1);
    let states = fx.manager.enable(enable_request(&sites)).await.unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    assert_eq!(
        fx.fake.requests().len(),
        1,
        "same block: no second hosts.set_block"
    );
    // A different set of sites does prompt.
    fx.manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(fx.fake.requests().len(), 2);
    assert_eq!(hosts_lines(&fx.os).len(), 2);
}

/// Every `hosts.set_block` takes `delay` (a UAC prompt the user takes a while to accept).
fn slow_hosts(delay: Duration) -> Arc<FakeElevation> {
    Arc::new(FakeElevation::answering(move |req| {
        if matches!(req.op, HelperOp::HostsSetBlock { .. }) {
            std::thread::sleep(delay);
        }
        Ok(HelperResponse::success(req, "fake helper: done"))
    }))
}

async fn wait_requests(fake: &FakeElevation, n: usize) {
    for _ in 0..400 {
        if fake.requests().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the helper never got request #{n}");
}

#[tokio::test]
async fn disable_during_a_site_sync_leaves_no_block() {
    // The sync is waiting for its prompt when `disable` arrives: disable waits for its
    // turn and then removes what the sync wrote.
    let fake = slow_hosts(Duration::from_millis(500));
    let os = MemoryPolicy::new(fake.clone()).with_group_policy_table();
    let fx = fixture_os(fake, os);
    fx.manager.enable(enable_request(&[])).await.unwrap();
    let sync = {
        let manager = fx.manager.clone();
        tokio::spawn(async move { manager.sync_sites(&["loja.test".into()]).await })
    };
    wait_requests(&fx.fake, 1).await;
    fx.manager.disable(&tlds()).await.unwrap();
    assert!(sync.await.unwrap().unwrap(), "the sync finished its write");
    assert_eq!(*fx.os.hosts.lock().unwrap(), STOCK_HOSTS, "no leftover");
    assert!(fx.manager.hosts_mode_tlds().is_empty());
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect();
    assert_eq!(names, ["hosts.set_block", "hosts.set_block"]);
}

#[tokio::test]
async fn a_site_sync_during_disable_does_not_write_the_block_back() {
    // `disable` is waiting for its prompt when a site change arrives: the sync waits for
    // its turn, sees hosts mode cleared and writes nothing.
    let fake = slow_hosts(Duration::from_millis(500));
    let os = MemoryPolicy::new(fake.clone()).with_group_policy_table();
    let fx = fixture_os(fake, os);
    fx.manager
        .enable(enable_request(&["loja.test"]))
        .await
        .unwrap();
    assert_eq!(hosts_lines(&fx.os).len(), 2);
    let disable = {
        let manager = fx.manager.clone();
        tokio::spawn(async move { manager.disable(&tlds()).await })
    };
    wait_requests(&fx.fake, 2).await;
    let wrote = fx
        .manager
        .sync_sites(&["loja.test".into(), "blog.test".into()])
        .await
        .unwrap();
    assert!(!wrote);
    disable.await.unwrap().unwrap();
    assert_eq!(*fx.os.hosts.lock().unwrap(), STOCK_HOSTS);
    assert_eq!(fx.fake.requests().len(), 2, "no write after disable");
}
