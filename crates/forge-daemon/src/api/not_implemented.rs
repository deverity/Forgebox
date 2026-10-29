//! Methods of the contract whose backend is a later roadmap task (docs/10). They answer
//! the stable error `not_implemented` naming the task, so clients can tell "not yet"
//! from "broken"; nothing panics.

use std::sync::Arc;

use forge_core::error::ForgeError;
use forge_ipc::server::{Handler, Router};
use futures::FutureExt;

/// `(method, roadmap task, what is missing)`.
pub const NOT_IMPLEMENTED: &[(&str, &str, &str)] = &[
    (
        "tools.add_local",
        "M3-E1",
        "installing a local zip or folder as a tool version",
    ),
    (
        "services.set",
        "M2-E2",
        "quick settings written into the managed config files",
    ),
    ("php.ext_enable", "M2-E2", "per-version extension toggles"),
    ("php.ext_disable", "M2-E2", "per-version extension toggles"),
    ("php.xdebug", "M2-E2", "Xdebug toggle per PHP version"),
    (
        "sites.share",
        "M3-E3",
        "public tunnels (cloudflared, ngrok, expose)",
    ),
    ("sites.new", "M3-E2", "project scaffolding from a stack"),
    ("db.list", "M2-E3-T5", "database management"),
    ("db.create", "M2-E3-T5", "database management"),
    ("db.drop", "M2-E3-T5", "database management"),
    ("db.export", "M2-E3-T6", "database export"),
    ("db.import", "M2-E3-T6", "database import"),
    ("db.shell", "M2-E3-T5", "database shell"),
    ("db.url", "M2-E3-T5", "database URLs"),
    ("db.open", "M3-E1", "database GUI launch"),
    (
        "net.lan_expose",
        "M3-E3",
        "LAN exposure with firewall rules",
    ),
    (
        "net.lan_unexpose",
        "M3-E3",
        "LAN exposure with firewall rules",
    ),
    ("net.lan_qr", "M3-E3", "LAN QR codes"),
    ("mail.status", "M2-E3-T7", "Mailpit integration"),
    ("mail.open", "M2-E3-T7", "Mailpit integration"),
    ("mail.list", "M2-E3-T7", "Mailpit integration"),
    ("mail.show", "M2-E3-T7", "Mailpit integration"),
    ("mail.mark_read", "M2-E3-T7", "Mailpit integration"),
    ("mail.delete", "M2-E3-T7", "Mailpit integration"),
    ("mail.attachment", "M2-E3-T7", "Mailpit integration"),
    ("mail.clear", "M2-E3-T7", "Mailpit integration"),
    ("mail.env", "M2-E3-T7", "Mailpit integration"),
    ("logs.targets", "M2-E3-T8", "listing log targets"),
    ("shell.open", "M2-E3-T9", "external terminal launcher"),
    ("shell.open_editor", "M2-E3-T9", "editor launcher"),
    (
        "terminal.open",
        "M2-E3-T4",
        "daemon-hosted terminal sessions",
    ),
    (
        "terminal.list",
        "M2-E3-T4",
        "daemon-hosted terminal sessions",
    ),
    (
        "terminal.input",
        "M2-E3-T4",
        "daemon-hosted terminal sessions",
    ),
    (
        "terminal.resize",
        "M2-E3-T4",
        "daemon-hosted terminal sessions",
    ),
    (
        "terminal.close",
        "M2-E3-T4",
        "daemon-hosted terminal sessions",
    ),
    (
        "terminal.subscribe",
        "M2-E3-T4",
        "daemon-hosted terminal sessions",
    ),
    (
        "system.config_list",
        "M2-E3-T10",
        "configuration document with schema",
    ),
    ("tls.ca_uninstall", "M2-E3-T11", "removing the local CA"),
    (
        "services.kill_port_holder",
        "M2-E3-T11",
        "terminating a port holder",
    ),
    ("sites.env", "M2-E3-T11", "masked .env viewer"),
    ("stats.get", "M2-E5", "per-site metrics (forge-metrics)"),
    ("stats.top", "M2-E5", "per-site metrics (forge-metrics)"),
    ("stats.export", "M2-E5", "per-site metrics (forge-metrics)"),
    (
        "metrics.subscribe",
        "M2-E5",
        "per-site metrics stream (forge-metrics)",
    ),
    ("remote.pair", "M6-E3", "remote mode pairing"),
    ("remote.listen", "M6-E3", "remote mode listener"),
    ("remote.clients", "M6-E3", "remote mode clients"),
    ("remote.revoke", "M6-E3", "remote mode clients"),
    ("system.bundle_export", "M3-E3", "offline bundles"),
    ("system.bundle_import", "M3-E3", "offline bundles"),
    ("system.backup_now", "M3-E3", "database backups"),
    ("system.backup_schedule", "M3-E3", "scheduled backups"),
    (
        "system.self_update",
        "M4-E1",
        "signed self-update of the app",
    ),
];

/// Roadmap task of the editor and terminal targets of `sites.open` (the browser and the
/// file manager already work; this is a partial method, so it is not in the table).
pub const SITES_OPEN_EDITOR_TERMINAL_TASK: &str = "M2-E3-T9";

/// Roadmap task of `logs.read {before_seq}` (paging back; `logs.read` itself works, so it
/// is not in the table).
pub const LOGS_BEFORE_SEQ_TASK: &str = "M2-E3-T8";

/// The method names of the table, in table order: what `system.hello` reports as
/// `not_implemented` (API 1.1) so clients can disable the matching controls.
pub fn methods() -> Vec<String> {
    NOT_IMPLEMENTED
        .iter()
        .map(|(method, _, _)| (*method).to_owned())
        .collect()
}

/// The error answered by a method of the table. Its structured details are
/// `{reason: "not_implemented", args: {method, task, what}}` (ADR-0039).
pub fn error(method: &str) -> ForgeError {
    let (task, what) = NOT_IMPLEMENTED
        .iter()
        .find(|(name, _, _)| *name == method)
        .map_or(("?", "this method"), |(_, task, what)| (*task, *what));
    ForgeError::NotImplementedYet {
        method: method.to_owned(),
        task: task.to_owned(),
        what: what.to_owned(),
    }
}

/// Registers every method of [`NOT_IMPLEMENTED`] that has no handler yet.
pub fn register(router: &mut Router) {
    for (method, _, _) in NOT_IMPLEMENTED {
        if router.contains(method) {
            continue;
        }
        let name: &'static str = method;
        let handler: Handler =
            Arc::new(move |_params, _ctx| async move { Err(error(name)) }.boxed());
        if let Err(err) = router.handle_raw(name, handler) {
            tracing::error!(method = name, %err, "not_implemented route not registered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_a_method_of_the_contract_with_a_task() {
        for (method, task, _) in NOT_IMPLEMENTED {
            assert!(
                forge_ipc::api::method_info(method).is_some(),
                "{method} is not in METHODS"
            );
            assert!(task.starts_with('M'), "{method}: {task}");
        }
        let err = error("sites.share");
        assert_eq!(err.code(), "not_implemented");
        assert!(err.to_string().contains("M3-E3"));
        let details = err.details().unwrap();
        assert_eq!(details["reason"], "not_implemented");
        assert_eq!(details["args"]["method"], "sites.share");
        assert_eq!(details["args"]["task"], "M3-E3");
    }

    #[test]
    fn hello_reports_exactly_the_table() {
        let reported = methods();
        assert_eq!(reported.len(), NOT_IMPLEMENTED.len());
        for (method, _, _) in NOT_IMPLEMENTED {
            assert!(reported.iter().any(|m| m == method), "{method}");
        }
        // sites.open works for the browser and the file manager: not "not implemented".
        assert!(!reported.iter().any(|m| m == "sites.open"));
        // Sorted names would hide a duplicated row.
        let mut sorted = reported.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), reported.len(), "duplicated row");
    }

    #[test]
    fn e3_methods_name_their_task() {
        // The E3 contract (ADR-0040): each backend task owns its methods.
        for (method, task) in [
            ("db.list", "M2-E3-T5"),
            ("db.create", "M2-E3-T5"),
            ("db.drop", "M2-E3-T5"),
            ("db.url", "M2-E3-T5"),
            ("db.shell", "M2-E3-T5"),
            ("db.export", "M2-E3-T6"),
            ("db.import", "M2-E3-T6"),
            ("db.open", "M3-E1"),
            ("mail.status", "M2-E3-T7"),
            ("mail.open", "M2-E3-T7"),
            ("mail.list", "M2-E3-T7"),
            ("mail.show", "M2-E3-T7"),
            ("mail.mark_read", "M2-E3-T7"),
            ("mail.delete", "M2-E3-T7"),
            ("mail.attachment", "M2-E3-T7"),
            ("mail.clear", "M2-E3-T7"),
            ("mail.env", "M2-E3-T7"),
            ("logs.targets", "M2-E3-T8"),
            ("shell.open", "M2-E3-T9"),
            ("shell.open_editor", "M2-E3-T9"),
            ("terminal.open", "M2-E3-T4"),
            ("terminal.list", "M2-E3-T4"),
            ("terminal.input", "M2-E3-T4"),
            ("terminal.resize", "M2-E3-T4"),
            ("terminal.close", "M2-E3-T4"),
            ("terminal.subscribe", "M2-E3-T4"),
            ("system.config_list", "M2-E3-T10"),
            ("tls.ca_uninstall", "M2-E3-T11"),
            ("services.kill_port_holder", "M2-E3-T11"),
            ("sites.env", "M2-E3-T11"),
        ] {
            let err = error(method);
            assert_eq!(err.code(), "not_implemented", "{method}");
            assert!(
                err.to_string().contains(&format!("(roadmap {task})")),
                "{method}: {err}"
            );
            assert_eq!(err.details().unwrap()["args"]["task"], task, "{method}");
        }
        // terminal.unsubscribe is a server built-in (closes any stream of the connection).
        assert!(!methods().iter().any(|m| m == "terminal.unsubscribe"));
        assert!(crate::api::SERVER_BUILTINS.contains(&"terminal.unsubscribe"));
    }

    #[test]
    fn the_gui_fake_bridge_mirrors_the_table() {
        // apps/desktop's fakeBridge answers not_implemented from this fixture; a stale copy
        // would make the GUI tests disagree with the daemon.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/src/ipc/fixtures/not-implemented.json");
        let fixture: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let rows: Vec<(String, String, String)> = fixture
            .iter()
            .map(|row| {
                let field = |name: &str| row[name].as_str().unwrap_or_default().to_owned();
                (field("method"), field("task"), field("what"))
            })
            .collect();
        let table: Vec<(String, String, String)> = NOT_IMPLEMENTED
            .iter()
            .map(|(m, t, w)| ((*m).to_owned(), (*t).to_owned(), (*w).to_owned()))
            .collect();
        assert_eq!(
            rows, table,
            "update apps/desktop/src/ipc/fixtures/not-implemented.json"
        );
    }

    #[test]
    fn registered_routes_answer_not_implemented() {
        let mut router = Router::new();
        register(&mut router);
        for (method, _, _) in NOT_IMPLEMENTED {
            assert!(router.contains(method), "{method}");
        }
    }
}
