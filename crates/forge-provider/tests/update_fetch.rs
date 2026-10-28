//! `DownloadFetcher` (M1-E2-T5): the update engine's archive fetcher over the download
//! queue, against the local axum server of the download tests.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::jobs::{JobSpec, JobState};
use forge_manifest::Manifest;
use forge_manifest::schema::ToolManifest;
use forge_provider::download::Priority;
use forge_provider::provider::Release;
use forge_provider::update::{ArchiveFetcher, DownloadFetcher, FetchArchive};
use support::{Behavior, Harness, TestServer, payload, sha256_hex};

fn manifest() -> Arc<ToolManifest> {
    let text = r#"
apiVersion: forgebox.dev/v1
kind: Tool
metadata: { id: nginx, name: Nginx, category: webserver }
spec:
  platforms: [windows-x64]
  source:
    providers:
      - type: nginx-org
"#;
    match forge_manifest::load_str(text).unwrap() {
        Manifest::Tool(tool) => Arc::new(tool),
        _ => panic!("not a tool"),
    }
}

fn release(url: String, sha256: String, size: u64) -> Release {
    Release {
        version: "1.30.6".into(),
        url,
        sha256: Some(sha256),
        sha512: None,
        size: Some(size),
        variants: BTreeMap::new(),
        published_at: None,
        signature: None,
        file_name: "nginx-1.30.6.zip".into(),
        checksum_url: None,
        tags: Vec::new(),
    }
}

/// Runs the fetcher inside a job of the harness' registry (as the update job does).
async fn fetch(h: &Harness, fetcher: DownloadFetcher, release: Release) -> Result<PathBuf, String> {
    let manifest = manifest();
    let handle = h
        .jobs
        .spawn(JobSpec::new("tool.update test"), move |ctx| async move {
            fetcher
                .fetch(FetchArchive {
                    job: &ctx,
                    manifest: &manifest,
                    release: &release,
                    priority: Priority::AutoUpdate,
                })
                .await
        });
    let info = handle.wait(Duration::from_secs(30)).await.unwrap();
    match info.state {
        JobState::Done => Ok(serde_json::from_value(info.result.unwrap()).unwrap()),
        _ => Err(info.error.map(|e| e.code).unwrap_or_default()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_the_release_into_the_download_dir() {
    let data = payload(64 * 1024);
    let server = TestServer::start().await;
    server.add("/nginx-1.30.6.zip", Behavior::new(data.clone()));
    let h = Harness::new(|_| {});
    let dir = h.dir.path().join("tmp").join("downloads");
    let fetcher = DownloadFetcher::new(h.manager.clone(), &dir, Duration::from_secs(30));
    let url = server.url("/nginx-1.30.6.zip").to_string();
    let path = fetch(
        &h,
        fetcher.clone(),
        release(url.clone(), sha256_hex(&data), data.len() as u64),
    )
    .await
    .unwrap();
    assert_eq!(path, dir.join("nginx-1.30.6.zip"));
    assert_eq!(std::fs::read(&path).unwrap(), data);

    // A wrong checksum is a verification failure, not a file.
    let err = fetch(&h, fetcher, release(url, "0".repeat(64), data.len() as u64))
        .await
        .unwrap_err();
    assert_eq!(err, "verification_failed");
}
