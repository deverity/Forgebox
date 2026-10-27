//! `DnsPolicyManager` over an in-memory OS policy whose changes go through the real
//! `HelperClient` (validation included) and a `FakeElevation` (no UAC): NRPT first,
//! `hosts` fallback, Group Policy, declined prompts, events and their IPC contract.

mod support;

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use forge_core::event::{EventBus, EventKind, EventStream};
use forge_core::helper::{FakeElevation, HelperOp};
use forge_core::platform::NrptSource;
use forge_ipc::api::events::{DnsPolicyPayload, HelperOpPayload};
use forge_ipc::api::net::DnsPolicyState;
use forge_net::policy::{DnsPolicyManager, EnableRequest, PolicyState, TldPolicy};
use support::{MemoryPolicy, STOCK_HOSTS};

struct Fixture {
    fake: Arc<FakeElevation>,
    os: Arc<MemoryPolicy>,
    manager: DnsPolicyManager,
    events: EventStream,
}

fn fixture_with(fake: FakeElevation, gpo: bool) -> Fixture {
    let fake = Arc::new(fake);
    let mut os = MemoryPolicy::new(fake.clone());
    if gpo {
        os = os.with_rule(NrptSource::GroupPolicy, ".corp.example", "10.0.0.1");
    }
    let os = Arc::new(os);
    let bus = EventBus::new();
    let events = bus.subscribe();
    let manager = DnsPolicyManager::new(os.clone()).with_events(bus);
    Fixture {
        fake,
        os,
        manager,
        events,
    }
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

#[tokio::test]
async fn enable_adds_the_nrpt_rule_once() {
    let mut fx = fixture();
    let states = fx
        .manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: Some(loopback()),
            hosts: vec!["loja.test".into()],
        })
        .await
        .unwrap();
    assert_eq!(
        states,
        vec![TldPolicy {
            tld: ".test".into(),
            state: PolicyState::Applied,
            nameserver: Some(loopback()),
        }]
    );
    assert_eq!(
        fx.fake.ops(),
        vec![HelperOp::NrptAdd {
            namespace: ".test".into(),
            nameserver: loopback()
        }]
    );
    assert_eq!(fx.os.hosts_entries(), None, "hosts untouched");
    assert_eq!(fx.os.flushes.load(Ordering::Relaxed), 1);

    // Events, deserialized with the IPC contract types.
    let events = drain(&mut fx.events);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].0, EventKind::HelperOp);
    let op: HelperOpPayload = serde_json::from_value(events[0].1.clone()).unwrap();
    assert_eq!(
        (op.v, op.name.as_str(), op.ok, op.error),
        (1, "nrpt.add", true, None)
    );
    assert_eq!(events[1].0, EventKind::DnsPolicyApplied);
    let applied: DnsPolicyPayload = serde_json::from_value(events[1].1.clone()).unwrap();
    assert_eq!(applied.v, 1);
    assert_eq!(applied.tld, ".test");
    assert_eq!(applied.policy, DnsPolicyState::Applied);

    // Second time: already applied, no prompt.
    fx.manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: Some(loopback()),
            hosts: vec![],
        })
        .await
        .unwrap();
    assert_eq!(fx.fake.requests().len(), 1);

    // The resolver moved to 127.0.0.53: the rule is rewritten.
    let fallback: IpAddr = "127.0.0.53".parse().unwrap();
    let status = fx.manager.status(&tlds(), Some(fallback)).await.unwrap();
    assert_eq!(status[0].state, PolicyState::NotApplied, "points elsewhere");
    assert_eq!(status[0].nameserver, Some(loopback()));
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
    let entries = fx.os.hosts_entries().unwrap();
    let lines: Vec<String> = entries
        .iter()
        .map(|e| format!("{} {}", e.ip, e.host))
        .collect();
    assert_eq!(
        lines,
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
    let events = drain(&mut fx.events);
    let applied: DnsPolicyPayload =
        serde_json::from_value(events.last().unwrap().1.clone()).unwrap();
    assert_eq!(applied.policy, DnsPolicyState::HostsFallback);

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

    // Nothing valid to write is an error, not an empty block.
    let err = fx
        .manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: None,
            hosts: vec!["x.com".into()],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
}

#[tokio::test]
async fn group_policy_rules_force_the_hosts_fallback() {
    let fx = fixture_with(FakeElevation::accepting(), true);
    let states = fx
        .manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: Some(loopback()),
            hosts: vec!["loja.test".into()],
        })
        .await
        .unwrap();
    assert_eq!(states[0].state, PolicyState::HostsFallback);
    let names: Vec<&str> = fx.fake.ops().iter().map(HelperOp::name).collect::<Vec<_>>();
    assert_eq!(names, ["nrpt.add", "hosts.set_block"]);
}

#[tokio::test]
async fn a_declined_prompt_stops_everything() {
    let mut fx = fixture_with(FakeElevation::declining(), false);
    let err = fx
        .manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: Some(loopback()),
            hosts: vec!["loja.test".into()],
        })
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
    fx.manager
        .enable(EnableRequest {
            tlds: tlds(),
            nameserver: Some(loopback()),
            hosts: vec![],
        })
        .await
        .unwrap();
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
    let events = drain(&mut fx.events);
    let last: DnsPolicyPayload = serde_json::from_value(events.last().unwrap().1.clone()).unwrap();
    assert_eq!(last.policy, DnsPolicyState::NotApplied);

    // Nothing left to remove: no prompt.
    let before = fx.fake.requests().len();
    fx.manager.disable(&tlds()).await.unwrap();
    assert_eq!(fx.fake.requests().len(), before);
}
