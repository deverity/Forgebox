//! Real Windows values of `platform::SystemAppearance` and `process_creation_time`
//! (M2-E1-T5), checked against sources that do not share code with the implementation:
//!
//! * `os_build` against `HKLM\…\CurrentVersion\CurrentBuildNumber` (read by the case
//!   sensitivity diagnostics) and `is_win11` against it;
//! * `remote_session` against `%SESSIONNAME%` (`RDP-Tcp#n` in Remote Desktop, `Console`
//!   locally; absent in service sessions, then not compared).
//!
//! Exact values of a given machine are opt-in through the environment, so the test runs
//! everywhere: `FORGEBOX_EXPECT_OS_BUILD=19045` and `FORGEBOX_EXPECT_REMOTE_SESSION=1`.
#![cfg(windows)]

use std::time::{Duration, Instant, SystemTime};

use forge_core::platform::{self, AppearanceSnapshot, WIN11_FIRST_BUILD};

/// Budget of one `snapshot()` (acceptance criterion of M2-E1-T5).
const SNAPSHOT_BUDGET: Duration = Duration::from_millis(5);

fn snapshot() -> AppearanceSnapshot {
    platform::system_appearance().snapshot()
}

#[test]
fn snapshot_matches_independent_sources() {
    let snap = snapshot();
    println!(
        "appearance snapshot: {}",
        serde_json::to_string(&snap).unwrap()
    );

    let (r, g, b) = snap
        .accent_rgb()
        .unwrap_or_else(|| panic!("accent is not #RRGGBB: {snap:?}"));
    assert_eq!(snap.accent, format!("#{r:02X}{g:02X}{b:02X}"));

    let registry_build = platform::current()
        .case_sensitive_fs()
        .info(&std::env::temp_dir())
        .os_build
        .expect("CurrentBuildNumber");
    assert_eq!(snap.os_build, registry_build, "{snap:?}");
    assert!(snap.os_build >= 10240, "{snap:?}");
    assert_eq!(
        snap.is_win11,
        snap.os_build >= WIN11_FIRST_BUILD,
        "{snap:?}"
    );

    match std::env::var("SESSIONNAME") {
        Ok(name) if name.to_ascii_uppercase().starts_with("RDP-") => {
            assert!(snap.remote_session, "SESSIONNAME={name}, {snap:?}");
        }
        Ok(name) if name.eq_ignore_ascii_case("Console") => {
            assert!(!snap.remote_session, "SESSIONNAME={name}, {snap:?}");
        }
        other => println!("SESSIONNAME {other:?}: remote_session not cross-checked"),
    }

    if let Ok(expected) = std::env::var("FORGEBOX_EXPECT_OS_BUILD") {
        let expected: u32 = expected.parse().expect("FORGEBOX_EXPECT_OS_BUILD");
        assert_eq!(snap.os_build, expected, "{snap:?}");
        assert_eq!(snap.is_win11, expected >= WIN11_FIRST_BUILD, "{snap:?}");
    }
    if let Ok(expected) = std::env::var("FORGEBOX_EXPECT_REMOTE_SESSION") {
        assert_eq!(snap.remote_session, expected == "1", "{snap:?}");
    }
}

#[test]
fn snapshot_is_fast_and_stable() {
    let cold = Instant::now();
    let first = snapshot();
    let cold = cold.elapsed();

    let mut runs: Vec<Duration> = (0..21)
        .map(|_| {
            let started = Instant::now();
            let snap = snapshot();
            let elapsed = started.elapsed();
            // Nothing changes between back-to-back calls (unless the user flips a
            // setting right now; the build and session never change).
            assert_eq!(snap.os_build, first.os_build);
            assert_eq!(snap.remote_session, first.remote_session);
            elapsed
        })
        .collect();
    runs.sort();
    let median = runs[runs.len() / 2];
    let max = runs[runs.len() - 1];
    println!("snapshot(): cold {cold:?}, median {median:?}, max {max:?} over 21 runs");
    assert!(
        median < SNAPSHOT_BUDGET,
        "median {median:?} (runs {runs:?})"
    );
    assert!(cold < SNAPSHOT_BUDGET * 10, "cold call took {cold:?}");
}

#[test]
fn snapshot_runs_on_a_blocking_thread() {
    // How async callers use it (ADR-0011): the call is sync and goes to spawn_blocking.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let snap = runtime
        .block_on(async { tokio::task::spawn_blocking(snapshot).await })
        .unwrap();
    assert!(snap.accent_rgb().is_some(), "{snap:?}");
}

#[test]
fn creation_time_of_this_process_is_in_the_past() {
    let created = platform::process_creation_time(std::process::id()).unwrap();
    let now = SystemTime::now();
    assert!(created <= now, "{created:?} > {now:?}");
    // The test binary started moments ago, not in another epoch.
    let age = now.duration_since(created).unwrap();
    assert!(age < Duration::from_secs(3600), "process age {age:?}");
    println!("process age at the check: {age:?}");

    // A process started after this check has a later creation time.
    let child = std::process::Command::new(std::env::var_os("ComSpec").unwrap())
        .args(["/c", "ping", "-n", "2", "127.0.0.1", ">NUL"])
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let child_created = platform::process_creation_time(child_pid);
    let mut child = child;
    let _ = child.wait();
    let child_created = child_created.unwrap();
    assert!(child_created >= created, "{child_created:?} < {created:?}");
    assert!(child_created >= now - Duration::from_millis(50));

    let err = platform::process_creation_time(u32::MAX - 3).unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
}
