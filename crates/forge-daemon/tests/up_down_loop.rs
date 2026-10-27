//! Acceptance of M1-E5-T2: `forgeboxd` goes up and down cleanly N times in a row on the
//! same root, without leaks. Each round: start the real process, connect through the
//! named pipe, `system.hello`, `daemon.stop`, wait for exit 0. Checked every round: the
//! pipe is gone, no `.lock` file is left; at the end: no daemon process and no child of
//! any of them survives (`tasklist`, `Win32_Process.ParentProcessId`), the `seq` of the
//! events continued across restarts, and the test process' own working set did not grow
//! (client side: connections, subscriptions, handles).
//!
//! `up_down_loop` (10 rounds) runs with `cargo test`; the full acceptance is
//! `cargo test -p forge-daemon --test up_down_loop -- --ignored up_down_100_times --nocapture`.

mod common;

use std::time::Duration;

use forge_ipc::api::NoParams;
use forge_ipc::api::methods::daemon_status;

use common::*;

struct Round {
    ready: Duration,
    stop: Duration,
}

async fn run(rounds: usize) {
    let root = TestRoot::new();
    let me = std::process::id();
    let mut pids = Vec::new();
    let mut measured = Vec::new();
    let mut ws_after_warmup = None;
    let mut last_seq = 0u64;
    for round in 0..rounds {
        let mut daemon = spawn_daemon(&root, &[]);
        pids.push(daemon.pid());
        let (client, ready) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
        client.hello().await.unwrap();
        let status = client.call::<daemon_status>(NoParams {}).await.unwrap();
        assert_eq!(status.pid, daemon.pid());
        assert!(
            status.last_event_seq > last_seq,
            "round {round}: seq {} after {last_seq}",
            status.last_event_seq
        );
        last_seq = status.last_event_seq;
        let (exit, stop) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
        client.close();
        drop(client);
        assert!(
            exit.success(),
            "round {round}: exit {exit}\n{}",
            daemon.output()
        );
        assert!(
            !pipe_exists(&root.root).await,
            "round {round}: pipe still there"
        );
        assert!(
            root.stray_lock_files().is_empty(),
            "round {round}: {:?}",
            root.stray_lock_files()
        );
        measured.push(Round { ready, stop });
        if round == rounds.min(10) - 1 {
            ws_after_warmup = working_set_kib(me);
        }
    }
    let ws_end = working_set_kib(me);

    // Only the daemons of this run (another test may be running its own right now).
    let alive: Vec<u32> = pids_of("forgeboxd.exe")
        .into_iter()
        .filter(|p| pids.contains(p))
        .collect();
    assert!(alive.is_empty(), "daemons still running: {alive:?}");
    let orphans = children_of(&pids);
    assert!(orphans.is_empty(), "children left behind: {orphans:?}");

    let avg = |f: fn(&Round) -> Duration| {
        measured.iter().map(f).sum::<Duration>() / u32::try_from(measured.len()).unwrap()
    };
    let max = |f: fn(&Round) -> Duration| measured.iter().map(f).max().unwrap();
    println!(
        "rounds={rounds} ready_avg={:?} ready_max={:?} stop_avg={:?} stop_max={:?} \
         test_ws_after_warmup_kib={ws_after_warmup:?} test_ws_end_kib={ws_end:?} \
         last_event_seq={last_seq} daemons_alive=0 children_left=0",
        avg(|r| r.ready),
        max(|r| r.ready),
        avg(|r| r.stop),
        max(|r| r.stop),
    );
    if let (Some(a), Some(b)) = (ws_after_warmup, ws_end) {
        // Allocator noise is fine; a per-round leak of handles/buffers would show up as
        // growth proportional to the rounds.
        assert!(
            b <= a + 8 * 1024,
            "test process grew from {a} KiB to {b} KiB over {rounds} rounds"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn up_down_loop() {
    run(10).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "acceptance run (100 rounds, about a minute): --ignored up_down_100_times"]
async fn up_down_100_times() {
    run(100).await;
}
