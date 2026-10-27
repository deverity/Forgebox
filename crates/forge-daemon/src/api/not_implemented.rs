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
        "sites.open",
        "M2-E1",
        "opening a site in the browser/editor/terminal of the daemon's machine",
    ),
    (
        "sites.share",
        "M3-E3",
        "public tunnels (cloudflared, ngrok, expose)",
    ),
    ("sites.new", "M3-E2", "project scaffolding from a stack"),
    ("db.list", "M2-E3", "database management"),
    ("db.create", "M2-E3", "database management"),
    ("db.drop", "M2-E3", "database management"),
    ("db.export", "M2-E3", "database export"),
    ("db.import", "M2-E3", "database import"),
    ("db.shell", "M2-E3", "database shell"),
    ("db.url", "M2-E3", "database URLs"),
    ("db.open", "M2-E3", "database GUI launch"),
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
    ("mail.status", "M2-E3", "Mailpit integration"),
    ("mail.open", "M2-E3", "Mailpit integration"),
    ("mail.list", "M2-E3", "Mailpit integration"),
    ("mail.show", "M2-E3", "Mailpit integration"),
    ("mail.clear", "M2-E3", "Mailpit integration"),
    ("mail.env", "M2-E3", "Mailpit integration"),
    ("shell.open", "M2-E3", "integrated terminal"),
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
    ("system.import_laragon", "M1-E5-T5", "Laragon import"),
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

/// The error answered by a method of the table.
pub fn error(method: &str) -> ForgeError {
    let (task, what) = NOT_IMPLEMENTED
        .iter()
        .find(|(name, _, _)| *name == method)
        .map_or(("?", "this method"), |(_, task, what)| (*task, *what));
    ForgeError::NotImplemented(format!(
        "{method} is not implemented yet: {what} (roadmap {task})"
    ))
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
