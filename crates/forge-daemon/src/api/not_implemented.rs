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
        "sites.share",
        "M3-E3",
        "public tunnels (cloudflared, ngrok, expose)",
    ),
    ("sites.new", "M3-E2", "project scaffolding from a stack"),
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
    (
        "sites.pool_mode",
        "M2-E5-T11",
        "dedicated PHP pool per site",
    ),
    ("sites.metrics", "M2-E5-T11", "per-site metrics switch"),
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
        // sites.open works for every target: not "not implemented".
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
        let (method, task) = ("db.open", "M3-E1");
        let err = error(method);
        assert_eq!(err.code(), "not_implemented", "{method}");
        assert!(
            err.to_string().contains(&format!("(roadmap {task})")),
            "{method}: {err}"
        );
        assert_eq!(err.details().unwrap()["args"]["task"], task, "{method}");
        // The local launchers exist (M2-E3-T9).
        for method in ["shell.open", "shell.open_editor"] {
            assert!(!methods().iter().any(|m| m == method), "{method}");
        }
        // The security extras exist (M2-E3-T11).
        for method in ["tls.ca_uninstall", "services.kill_port_holder", "sites.env"] {
            assert!(!methods().iter().any(|m| m == method), "{method}");
        }
        // The terminal backend exists (M2-E3-T4); terminal.unsubscribe is a server
        // built-in (closes any stream of the connection).
        assert!(!methods().iter().any(|m| m.starts_with("terminal.")));
        assert!(crate::api::SERVER_BUILTINS.contains(&"terminal.unsubscribe"));
    }

    #[test]
    fn e2_methods_name_their_task() {
        // The E2 contract (ADR-0041): each backend task owns its methods.
        // Partial methods (they work without the new parameter) stay out of the table, and
        // so do the ones whose backend landed (M2-E2-T4: history and icons; M2-E2-T5:
        // releases and plans; M2-E2-T7: quick settings; M2-E2-T9: extensions and Xdebug).
        for method in [
            "tools.releases",
            "tools.plan",
            "tools.install",
            "tools.remove",
            "tools.history",
            "tools.icon",
            "services.create",
            "services.settings",
            "services.set",
            "services.update",
            "php.ext_enable",
            "php.ext_disable",
            "php.xdebug",
        ] {
            assert!(!methods().iter().any(|m| m == method), "{method}");
        }
        // Every E2 row names its task, not only the epic.
        for (method, task, _) in NOT_IMPLEMENTED {
            if task.starts_with("M2-E2") {
                assert!(task.starts_with("M2-E2-T"), "{method}: {task}");
            }
        }
    }

    #[test]
    fn e5_methods_name_their_task() {
        // The metrics contract (ADR-0044): each backend task owns its methods.
        for (method, task) in [
            ("sites.pool_mode", "M2-E5-T11"),
            ("sites.metrics", "M2-E5-T11"),
        ] {
            let err = error(method);
            assert_eq!(err.code(), "not_implemented", "{method}");
            assert!(
                err.to_string().contains(&format!("(roadmap {task})")),
                "{method}: {err}"
            );
            let details = err.details().unwrap();
            assert_eq!(details["args"]["task"], task, "{method}");
            assert_eq!(details["args"]["method"], method);
        }
        // The metrics backend exists (M2-E5-T8, T9); metrics.unsubscribe is a server built-in.
        for method in [
            "stats.get",
            "metrics.subscribe",
            "stats.series",
            "stats.routes",
            "stats.export",
            "stats.top",
        ] {
            assert!(!methods().iter().any(|m| m == method), "{method}");
        }
        assert!(crate::api::SERVER_BUILTINS.contains(&"metrics.unsubscribe"));
        // Every E5 row names its task, not only the epic.
        for (method, task, _) in NOT_IMPLEMENTED {
            if task.starts_with("M2-E5") {
                assert!(task.starts_with("M2-E5-T"), "{method}: {task}");
            }
        }
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
