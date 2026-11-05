//! Acceptance of M2-E5-T6 (attribution rules).

use std::collections::BTreeMap;

use forge_metrics::{AppSample, Bucket, PoolMode, PoolRoute, PoolSample, SiteLoad, attribute};

fn pool(name: &str, mode: PoolMode, cpu: f64, mem: u64, from: u64, to: u64) -> PoolSample {
    PoolSample {
        pool: name.into(),
        mode,
        cpu_pct: cpu,
        mem_bytes: mem,
        workers_active: 4,
        from_ms: from,
        to_ms: to,
    }
}

fn route(site: &str, pool: &str, from: u64, to: u64) -> PoolRoute {
    PoolRoute {
        site: site.into(),
        pool: pool.into(),
        from_ms: from,
        to_ms: to,
    }
}

fn loads(items: &[(&str, u64, u64)]) -> BTreeMap<String, SiteLoad> {
    items
        .iter()
        .map(|(s, ut, rt)| {
            (
                (*s).to_owned(),
                SiteLoad {
                    ut_sum_us: *ut,
                    rt_sum_us: *rt,
                },
            )
        })
        .collect()
}

fn app(site: &str, cpu: f64, mem: u64) -> AppSample {
    AppSample {
        site: site.into(),
        cpu_pct: cpu,
        mem_bytes: mem,
    }
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

#[test]
fn shared_pool_splits_by_ut_three_to_one() {
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![pool("php_8_3", PoolMode::Shared, 8.0, 400, 0, 10_000)],
        routes: vec![
            route("a", "php_8_3", 0, 10_000),
            route("b", "php_8_3", 0, 10_000),
        ],
        loads: loads(&[("a", 3_000_000, 0), ("b", 1_000_000, 0)]),
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    near(out.sites["a"].cpu_pct, 6.0);
    near(out.sites["b"].cpu_pct, 2.0);
    assert_eq!(out.sites["a"].mem_bytes, 300);
    assert_eq!(out.sites["b"].mem_bytes, 100);
    assert!(out.sites["a"].approx && out.sites["b"].approx);
    assert!(out.overhead.is_empty());
}

#[test]
fn rt_is_the_fallback_when_there_is_no_ut() {
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![pool("p", PoolMode::Shared, 4.0, 0, 0, 10_000)],
        routes: vec![route("a", "p", 0, 10_000), route("b", "p", 0, 10_000)],
        loads: loads(&[("a", 0, 100), ("b", 0, 300)]),
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    near(out.sites["a"].cpu_pct, 1.0);
    near(out.sites["b"].cpu_pct, 3.0);
}

#[test]
fn dedicated_pool_is_exact_and_apps_are_summed() {
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![pool(
            "d",
            PoolMode::Dedicated("a".into()),
            5.0,
            1000,
            0,
            10_000,
        )],
        apps: vec![app("a", 1.5, 200), app("a", 0.5, 300), app("c", 2.0, 50)],
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    near(out.sites["a"].cpu_pct, 7.0);
    assert_eq!(out.sites["a"].mem_bytes, 1500);
    assert!(!out.sites["a"].approx);
    assert_eq!(out.sites["a"].workers_active, 4);
    near(out.sites["c"].cpu_pct, 2.0);
    assert!(!out.sites["c"].approx);
}

#[test]
fn zero_traffic_shared_pool_is_overhead() {
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![pool("p", PoolMode::Shared, 1.0, 500, 0, 10_000)],
        routes: vec![route("a", "p", 0, 10_000)],
        loads: loads(&[("a", 0, 0)]),
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    assert!(out.sites.is_empty());
    assert_eq!(out.overhead.len(), 1);
    near(out.overhead[0].cpu_pct, 1.0);
    assert_eq!(out.overhead[0].mem_bytes, 500);
}

#[test]
fn a_dedicated_pool_without_traffic_still_belongs_to_its_site() {
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![pool(
            "d",
            PoolMode::Dedicated("a".into()),
            0.0,
            100,
            0,
            10_000,
        )],
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    assert_eq!(out.sites["a"].mem_bytes, 100);
}

#[test]
fn a_move_inside_the_window_is_attributed_by_time() {
    // Site a moves from php_8_2 to php_8_3 at 4 s; b stays on 8.3 for the whole bucket.
    // 8.2 lived 0-4 s at 10 % (4 % averaged over the bucket), 8.3 lived 4-10 s at 10 %.
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![
            pool("php_8_2", PoolMode::Shared, 10.0, 1000, 0, 4_000),
            pool("php_8_3", PoolMode::Shared, 10.0, 1000, 4_000, 10_000),
        ],
        routes: vec![
            route("a", "php_8_2", 0, 4_000),
            route("a", "php_8_3", 4_000, 10_000),
            route("b", "php_8_3", 0, 10_000),
        ],
        loads: loads(&[("a", 100, 0), ("b", 100, 0)]),
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    // 8.2: only a -> 4 %, 400 B. 8.3 scaled to 6 %, 600 B; a weighs 100*0.6, b 100.
    near(out.sites["a"].cpu_pct, 4.0 + 6.0 * 60.0 / 160.0);
    near(out.sites["b"].cpu_pct, 6.0 * 100.0 / 160.0);
    near(out.sites["a"].cpu_pct + out.sites["b"].cpu_pct, 10.0);
    assert_eq!(out.sites["a"].mem_bytes + out.sites["b"].mem_bytes, 1000);
}

#[test]
fn dedicated_pools_swapped_mid_window_sum_by_time() {
    let bucket = Bucket {
        length_ms: 10_000,
        pools: vec![
            pool("old", PoolMode::Dedicated("a".into()), 10.0, 100, 0, 5_000),
            pool(
                "new",
                PoolMode::Dedicated("a".into()),
                20.0,
                100,
                5_000,
                10_000,
            ),
        ],
        ..Bucket::default()
    };
    let out = attribute(&bucket);
    near(out.sites["a"].cpu_pct, 5.0 + 10.0);
    assert_eq!(out.sites["a"].mem_bytes, 100);
}

#[test]
fn empty_bucket_is_empty() {
    assert!(attribute(&Bucket::default()).sites.is_empty());
}
