//! CPU sampling of the pool (M2-E5-T6): workers that burn a known share of a CPU are
//! measured by `PoolStatus.cpu_pct` / `PoolHandle::subscribe_metrics()` within 10 % of what
//! Windows reports (`Get-Process`, the counters behind Task Manager), and statuses written
//! before the field existed still deserialize.
#![cfg(windows)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use forge_core::ids::VersionId;
use forge_core::platform::process_stats;
use forge_fcgi::{PhpPool, PoolDeps, PoolSpec, PoolStatus, PoolTiming, Recycle, Scaling};
use support::*;

const ASK: Duration = Duration::from_secs(30);

fn spec(root: &std::path::Path, burn_pct: u32) -> PoolSpec {
    let mut spec = PoolSpec::new(
        VersionId::new(),
        "8.3.99",
        "8.3",
        fake_php_cgi(),
        root.join("etc").join("php").join("8.3"),
    )
    .unwrap();
    spec.env.set("FAKE_PHP_VERSION", "8.3.99");
    spec.env.set("FAKE_BURN_PCT", burn_pct.to_string());
    spec.expect_php_version = Some("8.3.99".to_owned());
    spec.scaling = Scaling {
        min: 2,
        max: 2,
        idle_timeout: Duration::from_secs(60),
    };
    spec.recycle = Recycle {
        max_requests: 0,
        max_rss_mb: 0,
        max_age: None,
    };
    spec.timing = PoolTiming {
        tick: Duration::from_millis(100),
        start_timeout: Duration::from_secs(15),
        ..PoolTiming::default()
    };
    spec
}

/// Sum of the CPU time (100 ns ticks) of `pids` and the clock, read by PowerShell.
fn get_process(pids: &[u32]) -> (u128, u128) {
    let ids = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$p = Get-Process -Id {ids}; \
         $c = ($p | ForEach-Object {{ $_.TotalProcessorTime.Ticks }} | Measure-Object -Sum).Sum; \
         Write-Output ('{{0}} {{1}}' -f [DateTime]::UtcNow.Ticks, $c)"
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("running powershell.exe");
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut parts = text.split_whitespace();
    (
        parts.next().unwrap().parse().unwrap(),
        parts.next().unwrap().parse().unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_cpu_matches_get_process_within_ten_percent() {
    let ports = Arc::new(TestPorts::default());
    let (supervisor, _bus) = supervisor();
    let root = tempfile::tempdir().unwrap();
    let pool = PhpPool::spawn(
        spec(root.path(), 40),
        PoolDeps {
            supervisor,
            ports,
            listener: None,
        },
    )
    .unwrap();
    pool.start(ASK).await.unwrap();
    let status = pool.status(ASK).await.unwrap();
    let pids: Vec<u32> = status.workers.iter().filter_map(|w| w.pid).collect();
    assert_eq!(pids.len(), 2);

    let mut metrics = pool.subscribe_metrics();
    // Let the burn settle and the first windows pass.
    tokio::time::sleep(Duration::from_secs(3)).await;
    metrics.mark_unchanged();

    let reader = tokio::task::spawn_blocking({
        let pids = pids.clone();
        move || {
            let a = get_process(&pids);
            std::thread::sleep(Duration::from_secs(8));
            (a, get_process(&pids))
        }
    });
    let mut ours = Vec::new();
    while !reader.is_finished() {
        if tokio::time::timeout(Duration::from_millis(500), metrics.changed())
            .await
            .is_ok()
        {
            if let Some(pct) = metrics.borrow_and_update().cpu_pct {
                ours.push(pct);
            }
        }
    }
    let (a, b) = reader.await.unwrap();
    let cpus = f64::from(process_stats().logical_cpus());
    let os_pct = (b.1 - a.1) as f64 / (b.0 - a.0) as f64 * 100.0 / cpus;
    assert!(ours.len() >= 40, "{} samples", ours.len());
    let our_pct = ours.iter().sum::<f64>() / ours.len() as f64;
    println!(
        "pool CPU: ours {our_pct:.3} % of the machine ({:.1} % of one CPU), Get-Process {os_pct:.3} % ({:.1} % of one CPU); {} samples, {cpus} logical CPUs",
        our_pct * cpus,
        os_pct * cpus,
        ours.len()
    );
    assert!(
        (our_pct - os_pct).abs() <= os_pct * 0.10,
        "ours {our_pct} vs Get-Process {os_pct}"
    );

    // PoolStatus carries the same figure, the workers' sum.
    let status = pool.status(ASK).await.unwrap();
    let workers: f64 = status.workers.iter().filter_map(|w| w.cpu_pct).sum();
    let total = status.cpu_pct.expect("pool cpu_pct");
    assert!((total - workers).abs() < 1e-9);
    assert!(total >= 0.0); // the last 100 ms window may fall in the burner's sleep

    // A status written before the field existed still deserializes (field absent).
    let mut wire = serde_json::to_value(&status).unwrap();
    wire.as_object_mut().unwrap().remove("cpu_pct");
    for worker in wire["workers"].as_array_mut().unwrap() {
        worker.as_object_mut().unwrap().remove("cpu_pct");
    }
    let old: PoolStatus = serde_json::from_value(wire).unwrap();
    assert_eq!(old.cpu_pct, None);
    assert!(old.workers.iter().all(|w| w.cpu_pct.is_none()));

    pool.stop(ASK).await.unwrap();
}
