//! Case sensitivity per site (docs/15 §3.1): boot re-verification of the `Aa`/`aA` state,
//! `forge site case-sensitive on|off` and `forge site lint-case` as jobs, and the wire
//! form of the results and events (API contracts of `forge_ipc::api::fs_parity`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::event::{Event, EventBus, EventKind};
use forge_core::fs::case::{CaseConversions, CaseReasonCode};
use forge_core::jobs::{JobInfo, JobRegistry, JobState};
use forge_core::platform::{self, CaseFsInfo, CaseSensitiveFs, CaseSensitivitySupport};
use forge_core::store::Store;
use forge_ipc::api::events::FsCaseSensitivityPayload;
use forge_ipc::api::fs_parity::{CaseConvertDto, CaseLintReportDto, CaseSensitivityDto};
use forge_sites::lint_case::FixOptions;
use forge_sites::{CaseParity, SiteDraft, SiteRules, SiteService};
use serde_json::Value;

struct Env {
    _store: Store,
    bus: EventBus,
    jobs: JobRegistry,
    sites: SiteService,
}

fn env() -> Env {
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let jobs = JobRegistry::new(bus.clone());
    let sites = SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        bus.clone(),
        SiteRules::default(),
    );
    Env {
        _store: store,
        bus,
        jobs,
        sites,
    }
}

fn real_cs() -> Arc<dyn CaseSensitiveFs> {
    struct Real(platform::Platform);
    impl CaseSensitiveFs for Real {
        fn support(&self, dir: &Path) -> CaseSensitivitySupport {
            self.0.case_sensitive_fs().support(dir)
        }
        fn get(&self, dir: &Path) -> forge_core::Result<bool> {
            self.0.case_sensitive_fs().get(dir)
        }
        fn set(&self, dir: &Path, enabled: bool) -> forge_core::Result<()> {
            self.0.case_sensitive_fs().set(dir, enabled)
        }
        fn info(&self, dir: &Path) -> CaseFsInfo {
            self.0.case_sensitive_fs().info(dir)
        }
        fn capture_dir_meta(&self, dir: &Path) -> Option<platform::DirMeta> {
            self.0.case_sensitive_fs().capture_dir_meta(dir)
        }
        fn apply_dir_meta(&self, dir: &Path, meta: &platform::DirMeta) -> forge_core::Result<()> {
            self.0.case_sensitive_fs().apply_dir_meta(dir, meta)
        }
    }
    Arc::new(Real(platform::current()))
}

/// Whether `%TEMP%` can use the flag (else the test is skipped with a message).
fn supported(dir: &Path) -> bool {
    match real_cs().support(dir) {
        CaseSensitivitySupport::Supported => true,
        CaseSensitivitySupport::Unsupported(reason) => {
            if std::env::var("FORGEBOX_REQUIRE_CASE_SENSITIVE").as_deref() == Ok("1") {
                panic!("case sensitivity required but unsupported: {reason}");
            }
            eprintln!("SKIPPED: {reason}");
            false
        }
    }
}

/// A Laravel-ish project (`composer.json` + `public/`) at `dir`.
fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::create_dir_all(dir.join("app/Models")).unwrap();
    std::fs::write(dir.join("composer.json"), "{}\n").unwrap();
    std::fs::write(dir.join("artisan"), "#!/usr/bin/env php\n").unwrap();
    std::fs::write(dir.join("public/index.php"), "<?php\n").unwrap();
}

async fn add_site(sites: &SiteService, dir: &Path, host: &str) -> forge_sites::Site {
    let draft = SiteDraft::from_dir(dir, host, None).unwrap();
    sites.create(draft).await.unwrap().site
}

async fn wait(jobs: &JobRegistry, id: forge_core::ids::JobId) -> JobInfo {
    for _ in 0..1000 {
        if let Some(info) = jobs.get(id) {
            if info.state.is_terminal() {
                return info;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

fn drain(rx: &mut forge_core::event::EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

/// Decodes an `fs.case_sensitivity_*` payload as the API type, checking nothing is lost.
fn decode(event: &Event) -> FsCaseSensitivityPayload {
    let api: FsCaseSensitivityPayload = serde_json::from_value(event.payload.clone())
        .unwrap_or_else(|e| panic!("{}: {e}\n{}", event.kind, event.payload));
    assert_eq!(serde_json::to_value(&api).unwrap(), event.payload);
    assert_eq!(api.v, 1);
    api
}

#[tokio::test]
async fn reverify_stores_the_real_state_and_publishes_one_event_per_site() {
    let tmp = tempfile::tempdir().unwrap();
    if !supported(tmp.path()) {
        return;
    }
    let e = env();
    let conversions = CaseConversions::new(e.jobs.clone(), real_cs());
    let parity = CaseParity::new(e.sites.clone(), conversions.clone());

    // `cs`: created empty, flagged, then filled (every folder inherits the flag).
    let cs_dir = tmp.path().join("cs");
    std::fs::create_dir(&cs_dir).unwrap();
    real_cs().set(&cs_dir, true).unwrap();
    project(&cs_dir);
    // `plain`: an ordinary insensitive folder.
    let plain = tmp.path().join("plain");
    project(&plain);
    // `partial`: only the top folder flagged (what `fs case-sensitive enable` does).
    let partial = tmp.path().join("partial");
    project(&partial);
    real_cs().set(&partial, true).unwrap();

    let a = add_site(&e.sites, &cs_dir, "cs.test").await;
    let b = add_site(&e.sites, &plain, "plain.test").await;
    let c = add_site(&e.sites, &partial, "partial.test").await;
    assert!(
        a.case_sensitive && b.case_sensitive,
        "new sites default to true"
    );
    let mut rx = e.bus.subscribe();

    let checks = parity.reverify().await.unwrap();
    let by_host: HashMap<&str, _> = checks.iter().map(|c| (c.host.as_str(), c)).collect();
    assert!(by_host["cs.test"].enabled && !by_host["cs.test"].changed);
    assert!(!by_host["plain.test"].enabled && by_host["plain.test"].changed);
    assert_eq!(
        by_host["plain.test"].reason_code,
        Some(CaseReasonCode::Insensitive)
    );
    assert_eq!(
        by_host["partial.test"].reason_code,
        Some(CaseReasonCode::Partial)
    );
    // the project folder, not the document root
    assert!(
        by_host["cs.test"].dir.ends_with("/cs"),
        "{}",
        by_host["cs.test"].dir
    );

    assert!(e.sites.get(a.id).await.unwrap().unwrap().case_sensitive);
    assert!(!e.sites.get(b.id).await.unwrap().unwrap().case_sensitive);
    assert!(!e.sites.get(c.id).await.unwrap().unwrap().case_sensitive);

    let events = drain(&mut rx);
    let fs_events: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind.as_str().starts_with("fs."))
        .collect();
    assert_eq!(fs_events.len(), 3);
    for ev in fs_events {
        let p = decode(ev);
        if p.site_id == Some(a.id) {
            assert_eq!(ev.kind, EventKind::FsCaseSensitivityApplied);
            assert!(p.reason.is_none());
        } else {
            assert_eq!(ev.kind, EventKind::FsCaseSensitivityUnavailable);
            assert!(p.reason_code.is_some() && p.reason.is_some());
        }
    }
    // the changed sites were announced
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == EventKind::SiteUpdated)
            .count(),
        2
    );
    // a second boot changes nothing
    let again = parity.reverify().await.unwrap();
    assert!(again.iter().all(|c| !c.changed));
}

#[tokio::test]
async fn unsupported_volume_is_reported_with_its_code() {
    struct Fat;
    impl CaseSensitiveFs for Fat {
        fn support(&self, _: &Path) -> CaseSensitivitySupport {
            CaseSensitivitySupport::Unsupported("file system FAT32".into())
        }
        fn get(&self, _: &Path) -> forge_core::Result<bool> {
            Ok(false)
        }
        fn set(&self, _: &Path, _: bool) -> forge_core::Result<()> {
            Err(forge_core::ForgeError::UnsupportedPlatform("FAT32".into()))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let e = env();
    let parity = CaseParity::new(
        e.sites.clone(),
        CaseConversions::new(e.jobs.clone(), Arc::new(Fat)),
    );
    project(&tmp.path().join("loja"));
    let site = add_site(&e.sites, &tmp.path().join("loja"), "loja.test").await;
    let checks = parity.reverify().await.unwrap();
    assert_eq!(checks[0].reason_code, Some(CaseReasonCode::Unsupported));
    assert!(checks[0].reason.as_deref().unwrap().contains("FAT32"));
    assert!(!e.sites.get(site.id).await.unwrap().unwrap().case_sensitive);

    // turning it on fails as a job and leaves the state alone
    let id = parity.set_site_case(site.id, true, None);
    let info = wait(&e.jobs, id).await;
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.unwrap().code, "unsupported_platform");
}

#[tokio::test]
async fn site_case_job_converts_the_project_folder_and_stores_the_state() {
    let tmp = tempfile::tempdir().unwrap();
    if !supported(tmp.path()) {
        return;
    }
    let e = env();
    let conversions = CaseConversions::new(e.jobs.clone(), real_cs());
    let parity = CaseParity::new(e.sites.clone(), conversions.clone());
    let dir = tmp.path().join("loja");
    project(&dir);
    let site = add_site(&e.sites, &dir, "loja.test").await;
    assert!(site.root.as_deref().unwrap().ends_with("public"));
    let site = e.sites.set_case_sensitive(site.id, false).await.unwrap();
    let mut rx = e.bus.subscribe();

    let id = parity.set_site_case(site.id, true, None);
    let info = wait(&e.jobs, id).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    assert_eq!(info.result.as_ref().unwrap()["case_sensitive"], true);
    // the whole project (not only public/) is case-sensitive now
    let flags = conversions.tree_flags(dir.clone(), 10, 1000).await.unwrap();
    assert_eq!(flags.sensitive, flags.directories);
    assert!(e.sites.get(site.id).await.unwrap().unwrap().case_sensitive);
    std::fs::write(dir.join("app/Models/A.txt"), "1").unwrap();
    std::fs::write(dir.join("app/Models/a.txt"), "2").unwrap();
    let events = drain(&mut rx);
    let applied = events
        .iter()
        .find(|e| e.kind == EventKind::FsCaseSensitivityApplied)
        .expect("fs.case_sensitivity_applied");
    assert_eq!(decode(applied).site_id, Some(site.id));
    assert!(events.iter().any(|e| e.kind == EventKind::JobProgress));

    // off is refused while names collide, then works
    let info = wait(&e.jobs, parity.set_site_case(site.id, false, None)).await;
    assert_eq!(info.error.unwrap().code, "conflict");
    assert!(e.sites.get(site.id).await.unwrap().unwrap().case_sensitive);
    std::fs::remove_file(dir.join("app/Models/a.txt")).unwrap();
    let info = wait(&e.jobs, parity.set_site_case(site.id, false, None)).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    assert!(!e.sites.get(site.id).await.unwrap().unwrap().case_sensitive);
    let events = drain(&mut rx);
    let off = events
        .iter()
        .find(|e| e.kind == EventKind::FsCaseSensitivityUnavailable)
        .expect("fs.case_sensitivity_unavailable");
    assert_eq!(decode(off).reason_code.as_deref(), Some("disabled"));
}

#[tokio::test]
async fn lint_site_job_scans_and_fixes() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env();
    let parity = CaseParity::new(
        e.sites.clone(),
        CaseConversions::new(e.jobs.clone(), real_cs()),
    );
    let dir = tmp.path().join("loja");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/case"),
        &dir,
    );
    let site = add_site(&e.sites, &dir, "loja.test").await;

    let info = wait(&e.jobs, parity.lint_site(site.id, None, None)).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    let report: CaseLintReportDto = serde_json::from_value(info.result.unwrap()).unwrap();
    assert_eq!(report.issues.len(), 3);
    assert!(report.root.ends_with("/loja"), "{}", report.root);

    let info = wait(
        &e.jobs,
        parity.lint_site(site.id, Some(FixOptions::default()), None),
    )
    .await;
    let report: CaseLintReportDto = serde_json::from_value(info.result.unwrap()).unwrap();
    assert_eq!(report.remaining, Some(0));
    assert!(report.issues.iter().all(|i| i.fixed));
}

/// `ConvertReport` and `CaseStatus` are the wire forms of `fs_parity.case_convert` and
/// `fs_parity.case_status`.
#[tokio::test]
async fn convert_and_status_results_are_the_api_contracts() {
    let tmp = tempfile::tempdir().unwrap();
    if !supported(tmp.path()) {
        return;
    }
    let e = env();
    let conversions = CaseConversions::new(e.jobs.clone(), real_cs());
    let dir = tmp.path().join("www");
    project(&dir.join("loja"));

    let status = conversions.status(dir.clone()).await.unwrap();
    let json = serde_json::to_value(&status).unwrap();
    let api: CaseSensitivityDto = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&api).unwrap(), json);
    assert_eq!(api.fs_type.as_deref(), Some("NTFS"));
    assert_eq!(api.win_version_ok, Some(true));

    let info = wait(&e.jobs, conversions.convert(dir.clone(), true, None)).await;
    let json: Value = info.result.unwrap();
    let api: CaseConvertDto = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&api).unwrap(), json);
    assert!(api.enabled && api.files == 3 && api.directories == 5);
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to: PathBuf = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}
