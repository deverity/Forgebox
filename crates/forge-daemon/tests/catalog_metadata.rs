//! Catalog metadata through the API (M2-E2-T4) against the real `forgeboxd` with the signed
//! icon fixture registry (`tests/fixtures/registry-icons`, a folder: no network at all):
//!
//! * `tools.search {text: ""}` lists in index order with `kind`, color, icon hash, platforms,
//!   sizes; `tools.info` fills the manifest metadata and `stack.tools` for a stack;
//! * `tools.icon` serves the verified SVG with its sha256, refuses the six malicious
//!   fixtures with `verification_failed`, and keeps working from the cache when the registry
//!   folder is gone (offline);
//! * `tools.history` maps the `update_history` rows: update, update, rollback.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::ids::ToolId;
use forge_core::store::{
    NewToolVersion, NewUpdateHistory, Store, ToolVersionRepo, UpdateHistoryRepo, UpdateStatus,
    UpdateTrigger, VersionSource, VersionState,
};
use forge_ipc::api::methods::{tools_history, tools_icon, tools_info, tools_search};
use forge_ipc::api::{
    CatalogKind, ToolHistoryParams, ToolInfoDto, ToolParams, ToolSearchParams, UpdateStatusDto,
    UpdateTriggerDto,
};
use forge_ipc::{ApiError, Client, ClientError};

use common::*;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/registry-icons")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn id(text: &str) -> ToolId {
    ToolId::new(text).unwrap()
}

fn api_error(err: ClientError) -> ApiError {
    match err {
        ClientError::Api(api) => api,
        other => panic!("not an API error: {other:?}"),
    }
}

async fn icon_error(client: &Client, tool: &str) -> ApiError {
    api_error(
        client
            .call::<tools_icon>(ToolParams { tool: id(tool) })
            .await
            .unwrap_err(),
    )
}

fn sha256_of_file(path: &Path) -> String {
    forge_manifest::index::sha256_hex(&std::fs::read(path).unwrap())
}

const MB: u64 = 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_info_and_icons_come_from_the_verified_registry() {
    let root = TestRoot::new();
    let registry = root.dir.path().with_extension("registry");
    copy_dir(&fixture(), &registry);
    let source = registry.to_string_lossy().into_owned();
    let mut daemon = spawn_daemon(&root, &["--registry", &source]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;

    // Index order: tools by id, then the stack; every entry says what it is.
    let found = client
        .call::<tools_search>(ToolSearchParams::default())
        .await
        .unwrap();
    let ids: Vec<&str> = found.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "evil-external",
            "evil-foreign",
            "evil-href",
            "evil-onload",
            "evil-script",
            "evil-style",
            "hello-db",
            "hello-web",
            "hello-stack",
        ]
    );
    let web = found
        .entries
        .iter()
        .find(|e| e.id == id("hello-web"))
        .unwrap();
    assert_eq!(web.kind, CatalogKind::Tool);
    assert_eq!(web.color.as_deref(), Some("#336699"));
    assert_eq!(web.platforms, ["windows-x64"]);
    assert!(web.native && web.official && !web.installed);
    assert_eq!(web.latest.as_deref(), Some("1.10.0"));
    assert_eq!(web.update_available, None);
    assert_eq!(web.size_estimate_bytes, Some(12 * MB));
    let web_sha = sha256_of_file(&fixture().join("icons/hello-web.svg"));
    assert_eq!(web.icon_sha256.as_deref(), Some(web_sha.as_str()));
    let db = found
        .entries
        .iter()
        .find(|e| e.id == id("hello-db"))
        .unwrap();
    assert_eq!(db.icon_sha256, None);
    assert_eq!(db.size_estimate_bytes, Some(30 * MB));
    let stack = found
        .entries
        .iter()
        .find(|e| e.id == id("hello-stack"))
        .unwrap();
    assert_eq!(stack.kind, CatalogKind::Stack);
    assert_eq!(stack.size_estimate_bytes, Some(42 * MB));
    assert_eq!(stack.platforms, ["windows-x64"]);
    assert!(
        found
            .entries
            .iter()
            .filter(|e| e.kind == CatalogKind::Tool)
            .count()
            == 8
    );

    // A text search still ranks.
    let ranked = client
        .call::<tools_search>(ToolSearchParams {
            text: "hello-db".into(),
            category: None,
        })
        .await
        .unwrap();
    assert_eq!(ranked.entries[0].id, id("hello-db"));

    // tools.info of a tool: Markdown, links, variants with labels, suggests.
    let info: ToolInfoDto = client
        .call::<tools_info>(ToolParams {
            tool: id("hello-web"),
        })
        .await
        .unwrap();
    assert_eq!(info.kind, CatalogKind::Tool);
    assert!(
        info.description_md
            .as_deref()
            .is_some_and(|d| d.starts_with("# Hello Web") && d.contains("**fixture**"))
    );
    assert_eq!(
        info.homepage.as_deref(),
        Some("https://example.org/hello-web")
    );
    assert_eq!(
        info.changelog_url.as_deref(),
        Some("https://example.org/hello-web/changelog")
    );
    assert_eq!(info.license.as_deref(), Some("MIT"));
    assert_eq!(info.suggests, [id("hello-db")]);
    assert_eq!(info.variants.len(), 1);
    let variant = &info.variants[0];
    assert_eq!(
        (variant.key.as_str(), variant.default.as_deref()),
        ("flavor", Some("lite"))
    );
    assert_eq!(variant.values, ["lite", "full"]);
    assert_eq!(variant.labels["full"], "Full build");
    assert!(!variant.derived);
    assert!(info.stack.is_none() && info.extends.is_none());

    // tools.info of a stack: what it installs.
    let info: ToolInfoDto = client
        .call::<tools_info>(ToolParams {
            tool: id("hello-stack"),
        })
        .await
        .unwrap();
    assert_eq!(info.kind, CatalogKind::Stack);
    assert!(info.tool.installed.is_empty());
    let tools = info.stack.expect("a stack carries stack.tools").tools;
    let listed: Vec<(&str, Option<&str>, Option<&str>)> = tools
        .iter()
        .map(|t| (t.id.as_str(), t.series.as_deref(), t.instance.as_deref()))
        .collect();
    assert_eq!(
        listed,
        [
            ("hello-web", Some("1.10"), None),
            ("hello-db", None, Some("default"))
        ]
    );

    // tools.icon: the valid icon round-trips with the right sha256.
    let icon = client
        .call::<tools_icon>(ToolParams {
            tool: id("hello-web"),
        })
        .await
        .unwrap();
    assert_eq!(icon.sha256, web_sha);
    assert_eq!(
        icon.svg.as_bytes(),
        std::fs::read(fixture().join("icons/hello-web.svg")).unwrap()
    );
    assert_eq!(
        forge_manifest::index::sha256_hex(icon.svg.as_bytes()),
        icon.sha256
    );

    // The six malicious fixtures are refused; the rest answers not_found.
    for evil in [
        "evil-script",
        "evil-foreign",
        "evil-onload",
        "evil-href",
        "evil-external",
        "evil-style",
    ] {
        let err = icon_error(&client, evil).await;
        assert_eq!(err.code, "verification_failed", "{evil}: {err:?}");
    }
    assert_eq!(icon_error(&client, "hello-db").await.code, "not_found");
    assert_eq!(icon_error(&client, "nonexistent").await.code, "not_found");

    stop(&client, &mut daemon, Duration::from_secs(30)).await;

    // Offline: the registry folder is gone, the next boot serves the verified cache.
    std::fs::remove_dir_all(&registry).unwrap();
    let mut daemon = spawn_daemon(&root, &["--registry", &source]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let icon = client
        .call::<tools_icon>(ToolParams {
            tool: id("hello-web"),
        })
        .await
        .unwrap();
    assert_eq!(icon.sha256, web_sha);
    let found = client
        .call::<tools_search>(ToolSearchParams::default())
        .await
        .unwrap();
    assert_eq!(found.entries.len(), 9);
    stop(&client, &mut daemon, Duration::from_secs(30)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_maps_update_update_rollback_rows() {
    let root = TestRoot::new();
    let tool = id("hello-web");
    let (first, second, rollback) = {
        std::fs::create_dir_all(root.root.db().parent().unwrap()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        // `update_history.tool_id` references `tools`: an installed version creates the row.
        ToolVersionRepo::new(store.conn())
            .record_install(
                &NewToolVersion {
                    tool_id: tool.clone(),
                    tool_name: "Hello Web".into(),
                    version: "1.2.0".into(),
                    canonical_name: "1.2.0".into(),
                    variant: BTreeMap::new(),
                    path: forge_core::paths::posix_string(&root.root.bin().join("hello-web/1.2.0")),
                    state: VersionState::Installed,
                    source: VersionSource::Registry,
                },
                None,
                None,
            )
            .unwrap();
        let repo = UpdateHistoryRepo::new(store.conn());
        let update = |from: &str, to: &str, series: &str| NewUpdateHistory {
            tool_id: tool.clone(),
            series: Some(series.to_owned()),
            from_version_id: None,
            to_version_id: None,
            from_version: Some(from.to_owned()),
            to_version: to.to_owned(),
            policy: Some("auto-patch".into()),
            triggered_by: UpdateTrigger::User,
            status: UpdateStatus::Started,
            job_id: None,
            rollback_of: None,
        };
        let first = repo.insert(&update("1.2.0", "1.2.1", "1.2")).unwrap();
        repo.finish(first, UpdateStatus::Completed, None, None)
            .unwrap();
        let other = repo.insert(&update("1.9.0", "1.10.0", "1.10")).unwrap();
        repo.finish(other, UpdateStatus::Failed, None, Some("download failed"))
            .unwrap();
        let second = repo.insert(&update("1.2.1", "1.2.2", "1.2")).unwrap();
        repo.finish(second, UpdateStatus::Completed, None, None)
            .unwrap();
        let rollback = repo
            .record_rollback(
                second,
                &NewUpdateHistory {
                    from_version: Some("1.2.2".into()),
                    to_version: "1.2.1".into(),
                    ..update("1.2.2", "1.2.1", "1.2")
                },
            )
            .unwrap();
        (first, second, rollback)
    };
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;

    let history = client
        .call::<tools_history>(ToolHistoryParams {
            tool: tool.clone(),
            series: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(history.tool, tool);
    let rows: Vec<_> = history
        .entries
        .iter()
        .map(|e| (e.trigger, e.status, e.to.as_str()))
        .collect();
    // Newest first: the rollback, the failed update of another series, the undone update
    // and the first one.
    assert_eq!(
        rows,
        [
            (
                UpdateTriggerDto::Rollback,
                UpdateStatusDto::Completed,
                "1.2.1"
            ),
            (UpdateTriggerDto::User, UpdateStatusDto::RolledBack, "1.2.2"),
            (UpdateTriggerDto::User, UpdateStatusDto::Failed, "1.10.0"),
            (UpdateTriggerDto::User, UpdateStatusDto::Completed, "1.2.1"),
        ]
    );
    assert_eq!(history.entries[0].id, rollback);
    assert_eq!(history.entries[0].rollback_of, Some(second));
    assert_eq!(history.entries[1].id, second);
    assert_eq!(history.entries[1].from.as_deref(), Some("1.2.1"));
    assert_eq!(history.entries[2].error.as_deref(), Some("download failed"));
    assert_eq!(history.entries[3].id, first);
    assert_eq!(
        history.entries[3].policy,
        Some(forge_ipc::api::UpdatePolicy::AutoPatch)
    );
    assert!(history.entries[3].finished_at.is_some());

    // Series filter and limit.
    let series = client
        .call::<tools_history>(ToolHistoryParams {
            tool: tool.clone(),
            series: Some("1.10".into()),
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(series.entries.len(), 1);
    let limited = client
        .call::<tools_history>(ToolHistoryParams {
            tool,
            series: None,
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(limited.entries.len(), 2);
    // A tool without history answers an empty list.
    let none = client
        .call::<tools_history>(ToolHistoryParams {
            tool: id("php"),
            series: None,
            limit: None,
        })
        .await
        .unwrap();
    assert!(none.entries.is_empty());

    stop(&client, &mut daemon, Duration::from_secs(30)).await;
}
