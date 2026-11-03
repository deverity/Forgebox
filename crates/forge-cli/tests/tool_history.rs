//! `forge tool info <tool> --history` (M2-E2-T4): the rows of `update_history` as a table
//! and as the `tools.history` document, against a real `forgeboxd`. The rows are seeded in
//! the store before the daemon boots (the update engine itself is `forge-provider`'s).

mod common;

use common::*;
use forge_ipc::base::ids::ToolId;
use forge_ipc::base::paths::ForgeRoot;
use forge_ipc::base::store::{
    NewToolVersion, NewUpdateHistory, Store, ToolVersionRepo, UpdateHistoryRepo, UpdateStatus,
    UpdateTrigger, VersionSource, VersionState,
};
use pretty_assertions::assert_eq;

/// Update 8.3.35 -> 8.3.36, update 8.3.36 -> 8.3.37 and the rollback of the second one.
fn seed(root: &TestRoot) {
    let forge_root = ForgeRoot::new(&root.path).unwrap();
    std::fs::create_dir_all(forge_root.db().parent().unwrap()).unwrap();
    let store = Store::open(&forge_root.db()).unwrap();
    let tool = ToolId::new("php").unwrap();
    ToolVersionRepo::new(store.conn())
        .record_install(
            &NewToolVersion {
                tool_id: tool.clone(),
                tool_name: "PHP".into(),
                version: "8.3.35".into(),
                canonical_name: "8.3.35-nts-vs16-x64".into(),
                variant: Default::default(),
                path: "bin/php/8.3.35-nts-vs16-x64".into(),
                state: VersionState::Installed,
                source: VersionSource::Registry,
            },
            None,
            None,
        )
        .unwrap();
    let repo = UpdateHistoryRepo::new(store.conn());
    let update = |from: &str, to: &str| NewUpdateHistory {
        tool_id: tool.clone(),
        series: Some("8.3".into()),
        from_version_id: None,
        to_version_id: None,
        from_version: Some(from.into()),
        to_version: to.into(),
        policy: Some("notify".into()),
        triggered_by: UpdateTrigger::User,
        status: UpdateStatus::Started,
        job_id: None,
        rollback_of: None,
    };
    let first = repo.insert(&update("8.3.35", "8.3.36")).unwrap();
    repo.finish(first, UpdateStatus::Completed, None, None)
        .unwrap();
    let second = repo.insert(&update("8.3.36", "8.3.37")).unwrap();
    repo.finish(second, UpdateStatus::Completed, None, None)
        .unwrap();
    repo.record_rollback(second, &update("8.3.37", "8.3.36"))
        .unwrap();
}

/// The `YYYY-MM-DD HH:MM` cell of every data row replaced by a fixed text of the same width.
fn without_times(table: &str) -> String {
    table
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i > 0 && line.len() >= 16 && line.as_bytes()[4] == b'-' {
                format!("YYYY-MM-DD HH:MM{}", &line[16..])
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn history_prints_update_update_rollback_in_both_languages_and_as_json() {
    let root = TestRoot::new(true);
    seed(&root);
    let _daemon = start_daemon(&root);

    let human = root.run(&["tool", "info", "php", "--history"]);
    human.assert_success();
    assert_eq!(
        without_times(&human.stdout),
        "\
QUANDO            DE      PARA    ORIGEM    ESTADO     NOTA
YYYY-MM-DD HH:MM  8.3.37  8.3.36  rollback  concluída
YYYY-MM-DD HH:MM  8.3.36  8.3.37  usuário   desfeita
YYYY-MM-DD HH:MM  8.3.35  8.3.36  usuário   concluída
"
    );

    let english = root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["tool", "info", "php", "--history"]);
    english.assert_success();
    let header = english.stdout.lines().next().unwrap();
    assert_eq!(
        header.split_whitespace().collect::<Vec<_>>(),
        ["WHEN", "FROM", "TO", "TRIGGER", "STATE", "NOTE"]
    );
    assert!(english.stdout.contains("rolled back"), "{}", english.stdout);

    let json = root.json(&["tool", "info", "php", "--history"]);
    let mut entries = json["entries"].as_array().unwrap().clone();
    assert_eq!(json["tool"], "php");
    for entry in &mut entries {
        let object = entry.as_object_mut().unwrap();
        // Absent values are `null` in the document (ids of removed versions, no error...).
        object.retain(|_, value| !value.is_null());
        for key in ["id", "rollback_of", "started_at", "finished_at"] {
            if let Some(value) = object.get_mut(key) {
                if !value.is_string() {
                    continue;
                }
                *value = format!("<{key}>").into();
            }
        }
    }
    assert_eq!(
        serde_json::Value::Array(entries),
        serde_json::json!([
            {"id": "<id>", "series": "8.3", "from": "8.3.37", "to": "8.3.36",
             "policy": "notify", "trigger": "rollback", "status": "completed",
             "rollback_of": "<rollback_of>", "started_at": "<started_at>",
             "finished_at": "<finished_at>"},
            {"id": "<id>", "series": "8.3", "from": "8.3.36", "to": "8.3.37",
             "policy": "notify", "trigger": "user", "status": "rolled_back",
             "started_at": "<started_at>", "finished_at": "<finished_at>"},
            {"id": "<id>", "series": "8.3", "from": "8.3.35", "to": "8.3.36",
             "policy": "notify", "trigger": "user", "status": "completed",
             "started_at": "<started_at>", "finished_at": "<finished_at>"}
        ])
    );

    // --series and --limit reach the daemon.
    let other = root.json(&["tool", "info", "php", "--history", "--series", "8.4"]);
    assert_eq!(other["entries"].as_array().unwrap().len(), 0);
    let one = root.json(&["tool", "info", "php", "--history", "--limit", "1"]);
    assert_eq!(one["entries"].as_array().unwrap().len(), 1);
    let empty = root.run(&["tool", "info", "nginx", "--history"]);
    empty.assert_success();
    assert_eq!(empty.stdout, "Nenhuma atualização registrada.\n");
}
