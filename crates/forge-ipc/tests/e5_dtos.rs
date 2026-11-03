//! The metrics contract (ADR-0044): every new DTO survives a serde round trip with the wire
//! shape clients rely on, every extended DTO still reads the JSON an API 1.0 peer sends
//! (missing fields take their defaults), and no DTO of the contract carries a path of the
//! daemon's machine (ADR-0009).

use std::collections::BTreeMap;
use std::fmt::Debug;

use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use forge_core::Timestamp;
use forge_ipc::api::*;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn now() -> Timestamp {
    ts("2026-11-03T12:00:00Z")
}

/// Serializes, checks that both the `Value` and the text forms read back equal, and
/// returns the JSON for shape assertions.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> Value {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value);
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value);
    json
}

fn from<T: DeserializeOwned>(json: Value) -> T {
    serde_json::from_value(json).unwrap()
}

/// Every string anywhere in `value` (keys included).
fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                strings(v, out);
            }
        }
        _ => {}
    }
}

/// Review checklist item of ADR-0044: no absolute path of the daemon's machine.
fn assert_no_absolute_path(what: &str, value: &Value) {
    let mut all = Vec::new();
    strings(value, &mut all);
    for s in all {
        let bytes = s.as_bytes();
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\');
        assert!(
            !drive && !s.starts_with("\\\\"),
            "{what} carries an absolute path: {s}"
        );
    }
}

/// `SiteStatsDto` exactly as an API 1.0 daemon sends it.
fn site_stats_1_0() -> Value {
    json!({
        "site_id": "01JAAAAAAAAAAAAAAAAAAAAAAA",
        "host": "loja.test",
        "window": "5m",
        "at": "2026-11-03T12:00:00Z",
        "requests": 1200,
        "rps": 4.0,
        "p50_ms": 12.5,
        "p90_ms": 40.0,
        "p99_ms": 120.0,
        "error_pct": 0.25,
        "cpu_pct": 3.0,
        "mem_bytes": 67108864,
        "approx": true,
        "sampled": false
    })
}

fn site_stats() -> SiteStatsDto {
    let mut stats: SiteStatsDto = from(site_stats_1_0());
    stats.from = Some(ts("2026-11-03T11:55:00Z"));
    stats.bytes = 9_000_000_000;
    stats.status_2xx = 1150;
    stats.status_3xx = 47;
    stats.status_5xx = 3;
    stats.php_p50_ms = Some(10.0);
    stats.php_p90_ms = Some(35.5);
    stats.php_p99_ms = Some(110.0);
    stats.workers_active = Some(2);
    stats.processes = Some(1);
    stats.last_request_at = Some(ts("2026-11-03T11:59:59.5Z"));
    stats
}

#[test]
fn site_stats_from_a_1_0_peer_reads_with_defaults() {
    let old: SiteStatsDto = from(site_stats_1_0());
    assert_eq!(old.requests, 1200);
    assert_eq!(old.from, None);
    assert_eq!(old.bytes, 0);
    assert_eq!(
        (
            old.status_1xx,
            old.status_2xx,
            old.status_3xx,
            old.status_4xx,
            old.status_5xx
        ),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(
        (old.php_p50_ms, old.php_p90_ms, old.php_p99_ms),
        (None, None, None)
    );
    assert_eq!((old.workers_active, old.processes), (None, None));
    assert_eq!(old.last_request_at, None);
    // A sample from a peer that predates the switch was collected: metrics were on.
    assert!(old.metrics_enabled);
    // The 1.0 fields keep their wire names.
    let json = round_trip(&old);
    for (key, value) in site_stats_1_0().as_object().unwrap() {
        assert_eq!(&json[key], value, "{key}");
    }
}

#[test]
fn extended_site_stats_round_trip() {
    let stats = site_stats();
    let json = round_trip(&stats);
    assert_eq!(json["bytes"], 9_000_000_000u64);
    assert_eq!(json["status_5xx"], 3);
    assert_eq!(json["php_p90_ms"], 35.5);
    assert_eq!(json["workers_active"], 2);
    assert_eq!(json["last_request_at"], "2026-11-03T11:59:59.5Z");
    assert_eq!(json["from"], "2026-11-03T11:55:00Z");
    assert_eq!(json["metrics_enabled"], true);
    let off = SiteStatsDto {
        metrics_enabled: false,
        ..stats
    };
    assert_eq!(round_trip(&off)["metrics_enabled"], false);
    // The stream item carries the same sample.
    let envelope = MetricsEnvelope {
        subscription_id: SubscriptionId::new(),
        seq: 7,
        sample: off,
    };
    assert_eq!(round_trip(&envelope)["seq"], 7);
}

#[test]
fn stats_params_take_an_optional_range() {
    // 1.0 shape: no from/to.
    let old: StatsParams = from(json!({"site": "loja.test", "window": "1h"}));
    assert_eq!((old.from, old.to), (None, None));
    assert_eq!(old.window, StatsWindow::OneHour);
    // Nothing at all: every field defaults.
    assert_eq!(from::<StatsParams>(json!({})), StatsParams::default());
    let params = StatsParams {
        site: None,
        window: StatsWindow::FiveMinutes,
        from: Some(ts("2026-11-02T12:00:00Z")),
        to: Some(now()),
    };
    let json = round_trip(&params);
    assert_eq!(json["from"], "2026-11-02T12:00:00Z");
    assert_eq!(json["to"], "2026-11-03T12:00:00Z");
}

#[test]
fn windows_pick_the_series_step_and_the_subscribe_cadence() {
    // (window, wire, span s, series step ms, points over the span, cadence ms)
    for (window, wire, span, step, points, cadence) in [
        (StatsWindow::OneMinute, "1m", 60, 2_000, 30, 2_000),
        (StatsWindow::FiveMinutes, "5m", 300, 2_000, 150, 2_000),
        (StatsWindow::OneHour, "1h", 3_600, 10_000, 360, 10_000),
        (StatsWindow::OneDay, "24h", 86_400, 60_000, 1_440, 60_000),
        (
            StatsWindow::SevenDays,
            "7d",
            604_800,
            3_600_000,
            168,
            60_000,
        ),
    ] {
        assert_eq!(round_trip(&window), wire);
        assert_eq!(window.seconds(), span, "{wire}");
        assert_eq!(window.series_step_ms(), step, "{wire}");
        assert_eq!(window.seconds() * 1000 / step, points, "{wire}");
        assert!(points <= SERIES_MAX_POINTS, "{wire}");
        assert_eq!(window.subscribe_cadence_ms(), cadence, "{wire}");
    }
}

#[test]
fn series_round_trips_with_points_and_markers() {
    let params: StatsSeriesParams = from(json!({"site": "loja.test"}));
    assert_eq!(params.range, StatsWindow::FiveMinutes);
    assert_eq!((params.from, params.to), (None, None));
    let params = StatsSeriesParams {
        site: SiteRef::from("loja.test"),
        range: StatsWindow::OneDay,
        from: Some(ts("2026-11-02T12:00:00Z")),
        to: None,
    };
    assert_eq!(round_trip(&params)["range"], "24h");

    let busy = SeriesPointDto {
        at: ts("2026-11-03T11:00:00Z"),
        requests: 600,
        rps: 10.0,
        bytes: 1 << 20,
        status_4xx: 6,
        status_5xx: 0,
        error_pct: 1.0,
        p50_ms: Some(12.0),
        p90_ms: Some(30.0),
        p99_ms: Some(95.5),
        cpu_pct: Some(1.5),
        mem_bytes: Some(80 << 20),
        workers_active: Some(2),
        approx: true,
        sampled: false,
    };
    // Dense series: a step without requests is present, with no percentiles.
    let idle = SeriesPointDto {
        at: ts("2026-11-03T11:01:00Z"),
        requests: 0,
        rps: 0.0,
        bytes: 0,
        status_4xx: 0,
        status_5xx: 0,
        error_pct: 0.0,
        p50_ms: None,
        p90_ms: None,
        p99_ms: None,
        cpu_pct: None,
        mem_bytes: None,
        workers_active: None,
        approx: false,
        sampled: false,
    };
    let marker = SeriesMarkerDto {
        seq: 4242,
        at: ts("2026-11-03T11:00:30Z"),
        kind: "site.php_changed".into(),
        args: BTreeMap::from([("from".into(), "8.3".into()), ("to".into(), "8.4".into())]),
    };
    let rendered = SeriesMarkerDto {
        seq: 4243,
        at: ts("2026-11-03T11:00:31Z"),
        kind: "config.rendered".into(),
        args: BTreeMap::from([
            ("owner".into(), "site:loja.test".into()),
            ("fs_path".into(), "etc/nginx/sites/loja.test.conf".into()),
        ]),
    };
    let series = SeriesDto {
        site_id: SiteId::new(),
        host: "loja.test".into(),
        range: StatsWindow::OneDay,
        step_ms: StatsWindow::OneDay.series_step_ms(),
        from: ts("2026-11-03T11:00:00Z"),
        to: ts("2026-11-03T11:02:00Z"),
        points: vec![busy, idle],
        markers: vec![marker, rendered],
    };
    let json = round_trip(&series);
    assert_eq!(json["step_ms"], 60_000);
    assert_eq!(json["points"][1]["p99_ms"], Value::Null);
    assert_eq!(json["points"][0]["mem_bytes"], 80 << 20);
    assert_eq!(json["markers"][0]["kind"], "site.php_changed");
    assert_eq!(json["markers"][0]["args"]["to"], "8.4");
    assert_no_absolute_path("SeriesDto", &json);
}

#[test]
fn routes_round_trip_with_recent_errors() {
    let params: StatsRoutesParams = from(json!({"site": "loja.test"}));
    assert_eq!(round_trip(&params), json!({"site": "loja.test"}));
    let routes = RoutesDto {
        site_id: SiteId::new(),
        host: "loja.test".into(),
        since: ts("2026-11-02T18:00:00Z"),
        at: now(),
        total: 250_000,
        routes: vec![RouteStatsDto {
            method: "GET".into(),
            route: "/users/{id}".into(),
            count: 12_000,
            count_min: 11_950,
            observed: 11_900,
            status_4xx: 12,
            status_5xx: 2,
            error_pct: 0.12,
            mean_ms: Some(18.2),
            p50_ms: Some(15.0),
            p90_ms: Some(31.0),
            p99_ms: Some(88.0),
        }],
        recent_errors: vec![
            RecentErrorDto {
                at: ts("2026-11-03T11:59:58Z"),
                status: 500,
                method: "POST".into(),
                route: "/checkout".into(),
                host: "loja.test".into(),
                duration_ms: 812.5,
                upstream_ms: Some(810.0),
                bytes: 512,
            },
            RecentErrorDto {
                at: ts("2026-11-03T11:59:00Z"),
                status: 404,
                method: "GET".into(),
                route: "/favicon.ico".into(),
                host: "admin.loja.test".into(),
                duration_ms: 0.4,
                upstream_ms: None,
                bytes: 0,
            },
        ],
    };
    let json = round_trip(&routes);
    assert_eq!(json["routes"][0]["route"], "/users/{id}");
    assert_eq!(json["routes"][0]["count_min"], 11_950);
    assert_eq!(json["recent_errors"][0]["status"], 500);
    assert_eq!(json["recent_errors"][1]["upstream_ms"], Value::Null);
    assert_no_absolute_path("RoutesDto", &json);
}

#[test]
fn top_gains_the_machine_and_reads_1_0_json() {
    let old: TopDto = from(json!({"processes": []}));
    assert_eq!(old.machine, None);
    let top = TopDto {
        processes: vec![ProcessUsageDto {
            pid: 4242,
            name: "nginx.exe".into(),
            owner_kind: PortOwnerKind::Instance,
            owner_id: "01JAAAAAAAAAAAAAAAAAAAAAAA".into(),
            owner_name: "nginx/default".into(),
            cpu_pct: 0.5,
            rss_bytes: 12 << 20,
        }],
        machine: Some(MachineStatsDto {
            at: now(),
            cpu_pct: Some(23.5),
            logical_cpus: 8,
            mem_total: 16 << 30,
            mem_available: 6 << 30,
            forgebox_cpu_pct: Some(1.25),
            forgebox_rss: 300 << 20,
            disks: vec![
                DiskSpaceDto {
                    role: DiskRole::Root,
                    total: 512 << 30,
                    free: 70 << 30,
                    available: 70 << 30,
                },
                DiskSpaceDto {
                    role: DiskRole::Data,
                    total: 1 << 40,
                    free: 300 << 30,
                    available: 290 << 30,
                },
            ],
        }),
    };
    let json = round_trip(&top);
    assert_eq!(json["machine"]["mem_total"], 16u64 << 30);
    assert_eq!(json["machine"]["disks"][1]["role"], "data");
    // First sample: no CPU yet.
    let mut first = top.machine.clone().unwrap();
    first.cpu_pct = None;
    first.forgebox_cpu_pct = None;
    assert_eq!(round_trip(&first)["cpu_pct"], Value::Null);
    assert_no_absolute_path("TopDto", &json);
}

/// `SiteDto` exactly as an API 1.0 daemon sends it.
fn site_1_0() -> Value {
    json!({
        "id": "01JAAAAAAAAAAAAAAAAAAAAAAA",
        "slug": "loja-test",
        "host": "loja.test",
        "aliases": [],
        "kind": "php",
        "root": "C:/www/loja/public",
        "driver": "laravel",
        "server": "nginx",
        "php": {"series": "8.3", "exact": null, "resolved": "8.3.36", "upstream": "php_8_3"},
        "upstream": null,
        "websocket": false,
        "secure": true,
        "http_redirect": true,
        "env": {},
        "xdebug": "inherit",
        "parked": false,
        "enabled": true,
        "case_sensitive": true,
        "url": "https://loja.test",
        "processes": [],
        "created_at": "2026-11-03T12:00:00Z",
        "updated_at": "2026-11-03T12:00:00Z"
    })
}

#[test]
fn site_gains_pool_mode_and_metrics_switch() {
    let old: SiteDto = from(site_1_0());
    assert_eq!(old.pool_mode, SitePoolMode::Shared);
    assert!(old.metrics_enabled);
    let mut site = old;
    site.pool_mode = SitePoolMode::Dedicated;
    site.metrics_enabled = false;
    site.php.as_mut().unwrap().upstream = Some("php_8_3__loja-test".into());
    let json = round_trip(&site);
    assert_eq!(json["pool_mode"], "dedicated");
    assert_eq!(json["metrics_enabled"], false);
    assert_eq!(json["php"]["upstream"], "php_8_3__loja-test");
    for (mode, wire) in [
        (SitePoolMode::Shared, "shared"),
        (SitePoolMode::Dedicated, "dedicated"),
    ] {
        assert_eq!(round_trip(&mode), wire);
    }
}

#[test]
fn pool_mode_and_metrics_params_round_trip() {
    let params = SitePoolModeParams {
        site: SiteRef::from("loja.test"),
        mode: SitePoolMode::Dedicated,
    };
    assert_eq!(
        round_trip(&params),
        json!({"site": "loja.test", "mode": "dedicated"})
    );
    let params = SiteMetricsParams {
        site: SiteRef::from("loja.test"),
        enabled: false,
    };
    assert_eq!(
        round_trip(&params),
        json!({"site": "loja.test", "enabled": false})
    );
    // Both fields are required: a switch without a value is a client bug.
    assert!(serde_json::from_value::<SiteMetricsParams>(json!({"site": "loja.test"})).is_err());
    assert!(serde_json::from_value::<SitePoolModeParams>(json!({"site": "loja.test"})).is_err());
}

#[test]
fn new_methods_are_typed_end_to_end() {
    use forge_ipc::api::methods::*;
    fn params<M: ApiMethod>() -> &'static str {
        M::INFO.params
    }
    assert_eq!(params::<stats_series>(), "StatsSeriesParams");
    assert_eq!(params::<stats_routes>(), "StatsRoutesParams");
    assert_eq!(params::<sites_pool_mode>(), "SitePoolModeParams");
    assert_eq!(params::<sites_metrics>(), "SiteMetricsParams");
}
