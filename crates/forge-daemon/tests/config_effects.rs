//! Preferences end to end (ADR-0040, M2-E3-T10) against the real `forgeboxd` on a temporary
//! root:
//!
//! * `system.config_list` returns the values of `forgebox.toml` over the defaults, the
//!   defaults and a JSON Schema that validates the values;
//! * two concurrent `system.config_set` on different keys both persist (no lost update);
//! * `dns.enabled` stops and restarts the resolver live and `config.changed` carries the keys;
//! * a TLD change reports `tls.ca_rotate` in `requires` and re-renders the web configs
//!   (`config.rendered`);
//! * every `fix_method` of the doctor is a method of the contract.
#![cfg(windows)]

mod common;

use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::method_info;
use forge_ipc::api::methods::{
    daemon_doctor, jobs_get, net_dns_status, sites_list, sites_park, system_config_get,
    system_config_list, system_config_set,
};
use forge_ipc::api::{
    ConfigGetParams, ConfigSetParams, DoctorParams, DoctorReportDto, EventFilter, EventsListParams,
    JobIdParams, JobState, NoParams, ParkParams,
};
use serde_json::{Value, json};

use common::*;

async fn set(client: &Client, key: &str, value: Value) -> forge_ipc::api::ConfigValueDto {
    client
        .call::<system_config_set>(ConfigSetParams {
            key: key.into(),
            value,
        })
        .await
        .unwrap()
}

async fn events_of(client: &Client, kind: &str) -> Vec<Value> {
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

async fn dns_running(client: &Client) -> bool {
    client
        .call::<net_dns_status>(NoParams {})
        .await
        .unwrap()
        .server_running
}

async fn wait_for<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The subset of JSON Schema `schemars` emits for the configuration (`$ref`, `type`,
/// `properties`, `items`, `oneOf`/`anyOf`, `enum`, `minimum`/`maximum`), enough to check
/// that the schema describes the values; no validator crate is a dependency of the project.
fn valid(root: &Value, schema: &Value, value: &Value) -> bool {
    if schema.as_bool().is_some_and(|b| b) {
        return true;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = root.pointer(reference.trim_start_matches('#'));
        return target.is_some_and(|t| valid(root, t, value));
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = schema.get(key).and_then(Value::as_array) {
            if !options.iter().any(|o| valid(root, o, value)) {
                return false;
            }
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            return false;
        }
    }
    if let Some(ty) = schema.get("type") {
        let names: Vec<&str> = match ty {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let ok = names.iter().any(|n| match *n {
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            "null" => value.is_null(),
            _ => false,
        });
        if !ok {
            return false;
        }
    }
    if let (Some(n), Some(min)) = (
        value.as_f64(),
        schema.get("minimum").and_then(Value::as_f64),
    ) {
        if n < min {
            return false;
        }
    }
    if let (Some(n), Some(max)) = (
        value.as_f64(),
        schema.get("maximum").and_then(Value::as_f64),
    ) {
        if n > max {
            return false;
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) {
        if !items.iter().all(|i| valid(root, item_schema, i)) {
            return false;
        }
    }
    if let (Some(object), Some(props)) = (
        value.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        for (key, child) in object {
            if let Some(child_schema) = props.get(key) {
                if !valid(root, child_schema, child) {
                    return false;
                }
            }
        }
    }
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_list_matches_the_file_and_the_schema_validates_it() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;

    set(&client, "tld", json!(".dev")).await;
    let doc = client
        .call::<system_config_list>(NoParams {})
        .await
        .unwrap();

    // Values: the file over the defaults.
    assert_eq!(doc.values["tld"], ".dev");
    assert_eq!(doc.values["dns"]["enabled"], false, "set by the test root");
    assert_eq!(doc.defaults["tld"], ".test");
    assert_eq!(doc.defaults["dns"]["enabled"], true);
    let on_disk =
        forge_core::config::ForgeConfig::load_or_default(&root.root.forgebox_toml()).unwrap();
    assert_eq!(doc.values, serde_json::to_value(&on_disk).unwrap());
    assert!(
        doc.set_keys.contains(&"tld".to_owned()),
        "{:?}",
        doc.set_keys
    );
    assert!(doc.set_keys.contains(&"dns.enabled".to_owned()));
    assert!(!doc.set_keys.contains(&"case_sensitive".to_owned()));

    // The schema validates the values and the defaults, and rejects a wrong type.
    assert!(
        valid(&doc.schema, &doc.schema, &doc.values),
        "values against the schema"
    );
    assert!(
        valid(&doc.schema, &doc.schema, &doc.defaults),
        "defaults against the schema"
    );
    assert!(!valid(&doc.schema, &doc.schema, &json!({ "tld": 5 })));
    assert!(!valid(
        &doc.schema,
        &doc.schema,
        &json!({ "dns": { "enabled": "yes" } })
    ));
    assert!(doc.schema["properties"]["tld"]["description"].is_string());

    stop(&client, &mut daemon, Duration::from_secs(60)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sets_of_different_keys_both_persist() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;

    for round in 0..5 {
        let (a, b) = (format!(".a{round}"), round % 2 == 0);
        let (first, second) = tokio::join!(
            set(&client, "tld", json!(a)),
            set(&client, "case_sensitive", json!(b)),
        );
        assert_eq!(first.value, json!(a));
        assert_eq!(second.value, json!(b));
        let disk =
            forge_core::config::ForgeConfig::load_or_default(&root.root.forgebox_toml()).unwrap();
        assert_eq!(disk.tld, a, "round {round}");
        assert_eq!(disk.case_sensitive, b, "round {round}");
    }
    let tld = client
        .call::<system_config_get>(ConfigGetParams { key: "tld".into() })
        .await
        .unwrap();
    assert_eq!(tld.value, ".a4");

    stop(&client, &mut daemon, Duration::from_secs(60)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dns_enabled_starts_and_stops_the_resolver_live() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    assert!(
        !dns_running(&client).await,
        "the test root boots without DNS"
    );

    set(&client, "dns.enabled", json!(true)).await;
    wait_for("the resolver to start", || async {
        dns_running(&client).await
    })
    .await;

    set(&client, "dns.enabled", json!(false)).await;
    wait_for("the resolver to stop", || async {
        !dns_running(&client).await
    })
    .await;

    set(&client, "dns.enabled", json!(true)).await;
    wait_for("the resolver to restart", || async {
        dns_running(&client).await
    })
    .await;

    // One config.changed per effective write, with the key; a write of the same value
    // changes nothing and publishes nothing.
    set(&client, "dns.enabled", json!(true)).await;
    let changed = events_of(&client, "config.changed").await;
    let keys: Vec<&Value> = changed.iter().map(|p| &p["keys"]).collect();
    assert_eq!(
        keys,
        [
            &json!(["dns.enabled"]),
            &json!(["dns.enabled"]),
            &json!(["dns.enabled"])
        ]
    );

    set(&client, "dns.enabled", json!(false)).await;
    stop(&client, &mut daemon, Duration::from_secs(60)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tld_change_requires_a_new_ca_and_moves_the_parked_sites() {
    let root = TestRoot::new();
    let parked = root.path().join("parked");
    std::fs::create_dir_all(parked.join("app")).unwrap();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    client
        .call::<sites_park>(ParkParams {
            dir: forge_core::paths::posix_string(&parked),
            tld: None,
        })
        .await
        .unwrap();
    let hosts = |client: Client| async move {
        client
            .call::<sites_list>(NoParams {})
            .await
            .unwrap()
            .sites
            .into_iter()
            .map(|s| s.host)
            .collect::<Vec<_>>()
    };
    assert!(hosts(client.clone()).await.contains(&"app.test".to_owned()));

    let value = set(&client, "tld", json!(".dev")).await;
    assert!(
        value.requires.contains(&"tls.ca_rotate".to_owned()),
        "{:?}",
        value.requires
    );
    assert!(!value.is_default);

    // Parked folders were reconciled against the new TLD (the site rules follow it).
    let now = hosts(client.clone()).await;
    assert!(now.contains(&"app.dev".to_owned()), "{now:?}");
    assert!(!now.contains(&"app.test".to_owned()), "{now:?}");

    let changed = events_of(&client, "config.changed").await;
    let last = changed.last().expect("config.changed");
    assert_eq!(last["keys"], json!(["tld"]));
    assert!(
        last["requires"]
            .as_array()
            .unwrap()
            .contains(&json!("tls.ca_rotate"))
    );

    // `config.rendered` on the re-render of a real nginx: web_nginx_php_online (ignored).
    // Not a TLD: nothing to do afterwards.
    let plain = set(&client, "case_sensitive", json!(false)).await;
    assert!(plain.requires.is_empty());

    stop(&client, &mut daemon, Duration::from_secs(60)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_fix_method_of_the_doctor_is_a_known_method() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let job = client
        .call::<daemon_doctor>(DoctorParams::default())
        .await
        .unwrap();
    let done = finish(&client, job.id).await;
    let report: DoctorReportDto = serde_json::from_value(done.result.expect("report")).unwrap();
    assert!(!report.checks.is_empty());
    for check in &report.checks {
        if let Some(method) = &check.fix_method {
            assert!(method_info(method).is_some(), "{}: {method}", check.id);
        }
    }
    stop(&client, &mut daemon, Duration::from_secs(60)).await;
}

async fn finish(client: &Client, id: forge_core::ids::JobId) -> forge_ipc::api::JobDto {
    let deadline = Instant::now() + Duration::from_secs(120);
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
