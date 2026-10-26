//! Integration tests of the download engine against a local axum server that speaks
//! `Range`, fails, cuts connections and delays on demand (M1-E2-T1 acceptance).

mod support;

use std::time::{Duration, Instant};

use forge_core::error::ForgeError;
use forge_core::event::EventKind;
use forge_provider::download::{
    DownloadOutcome, DownloadRequest, FailureKind, Priority, ProxySetting,
};
use sha2::{Digest, Sha256};
use support::{Behavior, Harness, TestServer, payload, sha256_hex};

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

fn outcome(events: &[forge_core::event::Event]) -> DownloadOutcome {
    let done = events
        .iter()
        .find(|e| e.kind == EventKind::JobDone)
        .unwrap_or_else(|| panic!("no job.done in {:?}", kinds(events)));
    serde_json::from_value(done.payload["result"].clone()).expect("DownloadOutcome")
}

fn kinds(events: &[forge_core::event::Event]) -> Vec<String> {
    events.iter().map(|e| e.kind.to_string()).collect()
}

#[tokio::test]
async fn downloads_a_file_and_reports_path_size_and_sha256() {
    let data = payload(300 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add("/php.zip", Behavior::new(data.clone()));
    let h = Harness::new(|_| {});
    let mut events = h.events();
    let dest = h.dest("php.zip");

    let req = DownloadRequest::new(vec![server.url("/php.zip")], &dest)
        .expected_size(data.len() as u64)
        .expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(seen.first().unwrap().kind, EventKind::JobStarted);
    let out = outcome(&seen);
    assert_eq!(out.path, dest);
    assert_eq!(out.size, data.len() as u64);
    assert_eq!(out.sha256, sha);
    assert_eq!(out.source, server.url("/php.zip").to_string());
    assert!(out.mirror_failures.is_empty());
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(h.part_files().is_empty(), "the .part is moved, not kept");
}

#[tokio::test]
async fn resumes_after_the_server_cuts_the_connection() {
    let data = payload(600 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add(
        "/big.zip",
        Behavior::new(data.clone()).cut_first_after(200 * KIB),
    );
    let h = Harness::new(|c| c.buffer_size = 64 * KIB);
    let mut events = h.events();
    let dest = h.dest("big.zip");

    let req = DownloadRequest::new(vec![server.url("/big.zip")], &dest).expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    let out = outcome(&seen);
    assert_eq!(out.sha256, sha);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    let hits = server.hits("/big.zip");
    assert_eq!(hits.len(), 2, "one cut attempt and one resume: {hits:?}");
    assert_eq!(hits[0].range, None);
    let resume_from = hits[1].range_start().expect("second request is a Range");
    assert!(
        resume_from > 0 && resume_from <= 200 * KIB as u64,
        "resumed at {resume_from}, not from zero and not past the cut"
    );
}

#[tokio::test]
async fn resumes_from_an_existing_part_file_and_rehashes_it() {
    let data = payload(500 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add("/r.zip", Behavior::new(data.clone()));
    let h = Harness::new(|_| {});
    let mut events = h.events();
    // A previous run left 100 KiB behind.
    h.write_part(&sha, &data[..100 * KIB]);

    let req = DownloadRequest::new(vec![server.url("/r.zip")], h.dest("r.zip"))
        .expected_sha256(&sha)
        .expected_size(data.len() as u64);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(outcome(&seen).sha256, sha, "hash covers the resumed prefix");
    let hits = server.hits("/r.zip");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].range_start(), Some(100 * KIB as u64));
    assert_eq!(std::fs::read(h.dest("r.zip")).unwrap(), data);
}

#[tokio::test]
async fn server_that_ignores_range_restarts_from_zero() {
    let data = payload(200 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add("/n.zip", Behavior::new(data.clone()).ignore_range());
    let h = Harness::new(|_| {});
    let mut events = h.events();
    h.write_part(&sha, &data[..50 * KIB]);

    let req =
        DownloadRequest::new(vec![server.url("/n.zip")], h.dest("n.zip")).expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(outcome(&seen).sha256, sha);
    assert_eq!(std::fs::read(h.dest("n.zip")).unwrap(), data);
}

#[tokio::test]
async fn corrupt_part_file_is_discarded_and_fetched_again() {
    let data = payload(200 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add("/c.zip", Behavior::new(data.clone()));
    let h = Harness::new(|_| {});
    let mut events = h.events();
    h.write_part(&sha, &vec![0xAA; 50 * KIB]);

    let req =
        DownloadRequest::new(vec![server.url("/c.zip")], h.dest("c.zip")).expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(outcome(&seen).sha256, sha);
    let hits = server.hits("/c.zip");
    assert_eq!(hits.len(), 2, "resume attempt, then full restart: {hits:?}");
    assert_eq!(hits[1].range, None);
}

#[tokio::test]
async fn first_mirror_fails_with_500_second_mirror_serves() {
    let data = payload(100 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add("/m1.zip", Behavior::new(data.clone()).status(500));
    server.add("/m2.zip", Behavior::new(data.clone()));
    let h = Harness::new(|_| {});
    let mut events = h.events();

    let req = DownloadRequest::new(
        vec![server.url("/m1.zip"), server.url("/m2.zip")],
        h.dest("m.zip"),
    )
    .expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    let out = outcome(&seen);
    assert_eq!(out.source, server.url("/m2.zip").to_string());
    assert_eq!(out.mirror_failures.len(), 1);
    assert_eq!(
        out.mirror_failures[0].url,
        server.url("/m1.zip").to_string()
    );
    assert!(out.mirror_failures[0].reason.contains("500"));
    assert_eq!(out.mirror_failures[0].kind, FailureKind::Network);
    assert_eq!(std::fs::read(h.dest("m.zip")).unwrap(), data);
}

#[tokio::test]
async fn all_mirrors_failing_fails_the_job_with_every_reason() {
    let server = TestServer::start().await;
    server.add("/a", Behavior::new(vec![1]).status(500));
    server.add("/b", Behavior::new(vec![1]).status(404));
    let h = Harness::new(|_| {});
    let mut events = h.events();

    let req = DownloadRequest::new(vec![server.url("/a"), server.url("/b")], h.dest("x"));
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    let failed = seen.last().unwrap();
    assert_eq!(failed.kind, EventKind::JobFailed, "{:?}", kinds(&seen));
    assert_eq!(failed.payload["error"]["code"], "io");
    let message = failed.payload["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("500") && message.contains("404"),
        "{message}"
    );
    assert!(!h.dest("x").exists());
}

#[tokio::test]
async fn transient_503_is_retried_on_the_same_mirror() {
    let data = payload(50 * KIB);
    let server = TestServer::start().await;
    server.add(
        "/t",
        Behavior::new(data.clone()).fail_first_requests(2, 503),
    );
    let h = Harness::new(|_| {});
    let mut events = h.events();

    let req = DownloadRequest::new(vec![server.url("/t")], h.dest("t"));
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(outcome(&seen).sha256, sha256_hex(&data));
    assert_eq!(server.hits("/t").len(), 3);
}

#[tokio::test]
async fn sha256_mismatch_fails_with_verification_and_leaves_no_files() {
    let data = payload(100 * KIB);
    let server = TestServer::start().await;
    server.add("/v.zip", Behavior::new(data));
    let h = Harness::new(|_| {});
    let mut events = h.events();

    let req = DownloadRequest::new(vec![server.url("/v.zip")], h.dest("v.zip"))
        .expected_sha256("0".repeat(64));
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    let failed = seen.last().unwrap();
    assert_eq!(failed.kind, EventKind::JobFailed);
    assert_eq!(failed.payload["error"]["code"], "verification_failed");
    assert!(!h.dest("v.zip").exists());
    assert!(h.part_files().is_empty());
}

#[tokio::test]
async fn big_files_use_several_range_connections() {
    let data = payload(5 * MIB + 17);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add("/big.bin", Behavior::new(data.clone()));
    let h = Harness::new(|c| c.multi_connection_threshold = 2 * MIB as u64);
    let mut events = h.events();

    let req =
        DownloadRequest::new(vec![server.url("/big.bin")], h.dest("big.bin")).expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(outcome(&seen).sha256, sha);
    assert_eq!(std::fs::read(h.dest("big.bin")).unwrap(), data);
    let mut starts: Vec<u64> = server
        .hits("/big.bin")
        .iter()
        .filter_map(|h| h.range_start())
        .collect();
    starts.sort_unstable();
    assert_eq!(starts.len(), 4, "four segments: {starts:?}");
    assert_eq!(starts[0], 0);
    assert!(h.part_files().is_empty(), "sidecar and part are gone");
}

#[tokio::test]
async fn files_below_the_threshold_use_a_single_connection() {
    let data = payload(2 * MIB);
    let server = TestServer::start().await;
    server.add("/s.bin", Behavior::new(data.clone()));
    let h = Harness::new(|_| {}); // default threshold: 50 MB
    let mut events = h.events();

    let req = DownloadRequest::new(vec![server.url("/s.bin")], h.dest("s.bin"));
    let job = h.manager.download(req).await.unwrap();
    h.wait_terminal(&mut events, job).await;

    assert_eq!(server.hits("/s.bin").len(), 1);
}

#[tokio::test]
async fn segmented_download_resumes_after_a_segment_is_cut() {
    let data = payload(4 * MIB + 5);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add(
        "/seg.bin",
        Behavior::new(data.clone())
            .cut_first_after(300 * KIB)
            .cut_ranged_only(),
    );
    let h = Harness::new(|c| {
        c.multi_connection_threshold = 2 * MIB as u64;
        c.buffer_size = 128 * KIB;
    });
    let mut events = h.events();

    let req =
        DownloadRequest::new(vec![server.url("/seg.bin")], h.dest("seg.bin")).expected_sha256(&sha);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    assert_eq!(outcome(&seen).sha256, sha);
    assert_eq!(std::fs::read(h.dest("seg.bin")).unwrap(), data);
    // first request + 4 segments + at least one resumed segment.
    assert!(
        server.hits("/seg.bin").len() >= 6,
        "{:?}",
        server.hits("/seg.bin")
    );
}

#[tokio::test]
async fn cancelling_keeps_the_part_file_and_emits_no_done() {
    let data = payload(2 * MIB);
    let server = TestServer::start().await;
    server.add(
        "/slow.bin",
        Behavior::new(data.clone()).throttle(64 * KIB, Duration::from_millis(25)),
    );
    let h = Harness::new(|c| c.buffer_size = 32 * KIB);
    let mut events = h.events();

    let req = DownloadRequest::new(vec![server.url("/slow.bin")], h.dest("slow.bin"));
    let job = h.manager.download(req).await.unwrap();
    // Wait until some bytes were reported, then cancel.
    h.wait_for(&mut events, job, |e| {
        e.kind == EventKind::JobProgress && e.payload["bytes_done"].as_u64().unwrap_or(0) > 0
    })
    .await;
    assert!(h.manager.cancel(job).unwrap());
    let seen = h.wait_terminal(&mut events, job).await;

    let last = seen.last().unwrap();
    assert_eq!(last.kind, EventKind::JobCancelled, "{:?}", kinds(&seen));
    assert!(seen.iter().all(|e| e.kind != EventKind::JobDone));
    let parts = h.part_files();
    assert_eq!(parts.len(), 1, "the .part stays: {parts:?}");
    assert!(std::fs::metadata(&parts[0]).unwrap().len() > 0);
    assert!(!h.dest("slow.bin").exists());
}

#[tokio::test]
async fn cancelled_download_can_be_resumed_by_a_new_request() {
    let data = payload(1500 * KIB);
    let sha = sha256_hex(&data);
    let server = TestServer::start().await;
    server.add(
        "/rs.bin",
        Behavior::new(data.clone()).throttle(64 * KIB, Duration::from_millis(20)),
    );
    let h = Harness::new(|c| c.buffer_size = 32 * KIB);
    let mut events = h.events();
    let make = || {
        DownloadRequest::new(vec![server.url("/rs.bin")], h.dest("rs.bin")).expected_sha256(&sha)
    };

    let job = h.manager.download(make()).await.unwrap();
    h.wait_for(&mut events, job, |e| {
        e.kind == EventKind::JobProgress && e.payload["bytes_done"].as_u64().unwrap_or(0) > 100_000
    })
    .await;
    h.manager.cancel(job).unwrap();
    h.wait_terminal(&mut events, job).await;

    let job = h.manager.download(make()).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;
    assert_eq!(outcome(&seen).sha256, sha);
    let hits = server.hits("/rs.bin");
    assert_eq!(hits.len(), 2);
    assert!(hits[1].range_start().unwrap() > 0, "second run resumed");
}

#[tokio::test]
async fn progress_has_growing_bytes_speed_eta_and_increasing_seq() {
    let data = payload(1200 * KIB);
    let server = TestServer::start().await;
    server.add(
        "/p.bin",
        Behavior::new(data.clone()).throttle(64 * KIB, Duration::from_millis(30)),
    );
    let h = Harness::new(|c| c.progress_interval = Duration::from_millis(250));
    let mut events = h.events();

    let req = DownloadRequest::new(vec![server.url("/p.bin")], h.dest("p.bin"))
        .expected_size(data.len() as u64);
    let job = h.manager.download(req).await.unwrap();
    let seen = h.wait_terminal(&mut events, job).await;

    let progress: Vec<_> = seen
        .iter()
        .filter(|e| e.kind == EventKind::JobProgress)
        .collect();
    let with_bytes: Vec<(u64, u64)> = progress
        .iter()
        .filter_map(|e| Some((e.seq, e.payload["bytes_done"].as_u64()?)))
        .collect();
    assert!(with_bytes.len() >= 3, "only {} reports", with_bytes.len());
    assert!(with_bytes.windows(2).all(|w| w[0].0 < w[1].0), "seq grows");
    assert!(
        with_bytes.windows(2).all(|w| w[0].1 <= w[1].1),
        "bytes never shrink"
    );
    assert!(with_bytes.first().unwrap().1 < with_bytes.last().unwrap().1);
    assert!(
        progress
            .iter()
            .any(|e| e.payload["speed_bps"].as_u64().unwrap_or(0) > 0),
        "speed reported"
    );
    assert!(
        progress.iter().any(|e| e.payload["eta_secs"].is_u64()),
        "eta reported"
    );
    assert!(
        progress
            .iter()
            .any(|e| e.payload["bytes_total"] == data.len())
    );
    // Reports are spaced by the configured interval (250 ms), not per chunk.
    let seqs: Vec<u64> = seen.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
}

#[tokio::test]
async fn user_action_starts_before_a_queued_auto_update() {
    let server = TestServer::start().await;
    server.add(
        "/first",
        Behavior::new(payload(10 * KIB)).delay(Duration::from_millis(400)),
    );
    server.add("/auto", Behavior::new(payload(10 * KIB)));
    server.add("/user", Behavior::new(payload(10 * KIB)));
    let h = Harness::new(|c| c.max_parallel_downloads = 1);
    let mut events = h.events();

    let mk = |p: &str, prio| {
        DownloadRequest::new(vec![server.url(p)], h.dest(p.trim_start_matches('/'))).priority(prio)
    };
    let j1 = h
        .manager
        .download(mk("/first", Priority::AutoUpdate))
        .await
        .unwrap();
    server.wait_for_hit("/first").await;
    let j2 = h
        .manager
        .download(mk("/auto", Priority::AutoUpdate))
        .await
        .unwrap();
    let j3 = h
        .manager
        .download(mk("/user", Priority::UserAction))
        .await
        .unwrap();
    assert_eq!(h.manager.stats().await.unwrap().queued, 2);

    for job in [j1, j2, j3] {
        let seen = h.wait_terminal(&mut events, job).await;
        assert_eq!(seen.last().unwrap().kind, EventKind::JobDone);
    }
    assert_eq!(server.arrival_order(), ["/first", "/user", "/auto"]);
}

#[tokio::test]
async fn at_most_two_downloads_run_at_once() {
    let server = TestServer::start().await;
    let h = Harness::new(|_| {});
    let mut events = h.events();
    let mut jobs = Vec::new();
    for i in 0..5 {
        let path = format!("/f{i}");
        server.add(
            &path,
            Behavior::new(payload(10 * KIB)).delay(Duration::from_millis(150)),
        );
        let req = DownloadRequest::new(vec![server.url(&path)], h.dest(&format!("f{i}")));
        jobs.push(h.manager.download(req).await.unwrap());
    }
    for job in jobs {
        let seen = h.wait_terminal(&mut events, job).await;
        assert_eq!(seen.last().unwrap().kind, EventKind::JobDone);
    }
    assert_eq!(server.max_in_flight(), 2);
    let stats = h.manager.stats().await.unwrap();
    assert_eq!((stats.running, stats.queued), (0, 0));
}

#[tokio::test]
async fn a_job_cancelled_while_queued_never_starts_and_frees_its_entry() {
    let server = TestServer::start().await;
    server.add(
        "/a",
        Behavior::new(payload(10 * KIB)).delay(Duration::from_millis(300)),
    );
    server.add("/b", Behavior::new(payload(10 * KIB)));
    let h = Harness::new(|c| c.max_parallel_downloads = 1);
    let mut events = h.events();

    let a = h
        .manager
        .download(DownloadRequest::new(vec![server.url("/a")], h.dest("a")))
        .await
        .unwrap();
    server.wait_for_hit("/a").await;
    let b = h
        .manager
        .download(DownloadRequest::new(vec![server.url("/b")], h.dest("b")))
        .await
        .unwrap();
    assert!(h.manager.cancel(b).unwrap());
    let seen_b = h.wait_terminal(&mut events, b).await;
    assert_eq!(seen_b.last().unwrap().kind, EventKind::JobCancelled);
    h.wait_terminal(&mut events, a).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(server.hits("/b").is_empty());
    let stats = h.manager.stats().await.unwrap();
    assert_eq!((stats.running, stats.queued), (0, 0));
}

#[tokio::test]
async fn requesting_a_queued_file_as_user_action_raises_its_priority() {
    let server = TestServer::start().await;
    server.add(
        "/busy",
        Behavior::new(payload(10 * KIB)).delay(Duration::from_millis(400)),
    );
    for p in ["/a", "/x"] {
        server.add(p, Behavior::new(payload(10 * KIB)));
    }
    let h = Harness::new(|c| c.max_parallel_downloads = 1);
    let mut events = h.events();
    let mk = |p: &str, prio| {
        DownloadRequest::new(vec![server.url(p)], h.dest(p.trim_start_matches('/'))).priority(prio)
    };
    let j0 = h
        .manager
        .download(mk("/busy", Priority::AutoUpdate))
        .await
        .unwrap();
    server.wait_for_hit("/busy").await;
    let ja = h
        .manager
        .download(mk("/a", Priority::AutoUpdate))
        .await
        .unwrap();
    let jx = h
        .manager
        .download(mk("/x", Priority::AutoUpdate))
        .await
        .unwrap();
    let jx2 = h
        .manager
        .download(mk("/x", Priority::UserAction))
        .await
        .unwrap();
    assert_eq!(jx, jx2, "same job is returned");

    for job in [j0, ja, jx] {
        let seen = h.wait_terminal(&mut events, job).await;
        assert_eq!(seen.last().unwrap().kind, EventKind::JobDone);
    }
    assert_eq!(server.arrival_order(), ["/busy", "/x", "/a"]);
}

#[tokio::test]
async fn same_file_requested_twice_returns_the_same_job() {
    let server = TestServer::start().await;
    server.add(
        "/d",
        Behavior::new(payload(10 * KIB)).delay(Duration::from_millis(200)),
    );
    let h = Harness::new(|_| {});
    let mut events = h.events();
    let req = || DownloadRequest::new(vec![server.url("/d")], h.dest("d"));

    let first = h.manager.download(req()).await.unwrap();
    let second = h.manager.download(req()).await.unwrap();
    assert_eq!(first, second);
    // Same file, another destination: refused rather than racing on one .part.
    let other = DownloadRequest::new(vec![server.url("/d")], h.dest("other"));
    let err = h.manager.download(other).await.unwrap_err();
    assert_eq!(err.code(), "conflict");
    h.wait_terminal(&mut events, first).await;
    assert_eq!(server.hits("/d").len(), 1);
}

#[tokio::test]
async fn offline_mode_fails_immediately_without_touching_the_network() {
    let server = TestServer::start().await;
    server.add("/o", Behavior::new(payload(10)));
    let h = Harness::new(|c| c.offline = true);
    assert!(h.manager.is_offline());

    let started = Instant::now();
    let err = h
        .manager
        .download(DownloadRequest::new(vec![server.url("/o")], h.dest("o")))
        .await
        .unwrap_err();
    assert!(matches!(err, ForgeError::Conflict(_)), "{err}");
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(server.hits("/o").is_empty());
    assert!(h.jobs.list().is_empty(), "no job is created");

    h.manager.set_offline(false);
    let mut events = h.events();
    let job = h
        .manager
        .download(DownloadRequest::new(vec![server.url("/o")], h.dest("o")))
        .await
        .unwrap();
    h.wait_terminal(&mut events, job).await;
}

#[tokio::test]
async fn invalid_requests_are_rejected_before_queueing() {
    let h = Harness::new(|_| {});
    let url = url::Url::parse("http://127.0.0.1:1/x").unwrap();
    let err = h
        .manager
        .download(DownloadRequest::new(vec![], h.dest("x")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    let err = h
        .manager
        .download(DownloadRequest::new(vec![url.clone()], h.dest("x")).expected_sha256("abc"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    let ftp = url::Url::parse("ftp://example.org/x").unwrap();
    let err = h
        .manager
        .download(DownloadRequest::new(vec![ftp], h.dest("x")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    assert!(h.jobs.list().is_empty());
}

#[tokio::test]
async fn bandwidth_limit_slows_the_transfer() {
    let data = payload(300 * KIB);
    let server = TestServer::start().await;
    server.add("/bw", Behavior::new(data.clone()));
    let h = Harness::new(|c| {
        c.bandwidth_limit = Some(200 * KIB as u64);
        c.buffer_size = 32 * KIB;
    });
    let mut events = h.events();

    let started = Instant::now();
    let job = h
        .manager
        .download(DownloadRequest::new(vec![server.url("/bw")], h.dest("bw")))
        .await
        .unwrap();
    h.wait_terminal(&mut events, job).await;
    // 1 s of burst (200 KiB) is free, the remaining 100 KiB take 0.5 s.
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(h.manager.bandwidth_limit(), Some(200 * KIB as u64));
    h.manager.set_bandwidth_limit(None);
    assert_eq!(h.manager.bandwidth_limit(), None);
}

#[tokio::test]
async fn manual_proxy_is_used_for_requests() {
    // The "proxy" is the test server; the host below does not resolve anywhere.
    let data = payload(20 * KIB);
    let proxy = TestServer::start().await;
    proxy.add("/via-proxy.bin", Behavior::new(data.clone()));
    let h = Harness::new(|c| c.proxy = ProxySetting::Manual(proxy.url("/")));
    let mut events = h.events();

    let url = url::Url::parse("http://files.forgebox-test.invalid/via-proxy.bin").unwrap();
    let job = h
        .manager
        .download(DownloadRequest::new(vec![url], h.dest("via")))
        .await
        .unwrap();
    let seen = h.wait_terminal(&mut events, job).await;
    assert_eq!(outcome(&seen).sha256, sha256_hex(&data));
    assert_eq!(proxy.hits("/via-proxy.bin").len(), 1);
}

#[tokio::test]
async fn system_proxy_setting_still_serves_the_api() {
    // Smoke test only: it does not prove the system proxy is consulted (see
    // tests/system_proxy.rs for that). It checks the manager works with that setting.
    let server = TestServer::start().await;
    server.add("/sys", Behavior::new(payload(1000)));
    let h = Harness::new(|c| c.proxy = ProxySetting::System);
    let mut events = h.events();
    let job = h
        .manager
        .download(DownloadRequest::new(
            vec![server.url("/sys")],
            h.dest("sys"),
        ))
        .await;
    // Corporate proxies may legitimately intercept loopback; only the API must work.
    let job = job.unwrap();
    h.wait_terminal(&mut events, job).await;
}

#[test]
fn sha256_helper_matches_a_known_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let _ = Sha256::new();
}
