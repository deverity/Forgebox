//! Wire types shared by daemon, CLI, GUI and MCP, exported to TypeScript by `cargo xtask gen-ts`.
//!
//! Every type registered in [`type_collection`] ends up in
//! `apps/desktop/src/ipc/generated.ts`. Types are remote-ready (ADR-0009): they never
//! assume client and daemon share a machine.

use serde::{Deserialize, Serialize};
use specta::{Type, Types};

/// Version of the IPC API spoken by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ApiVersion {
    pub major: u16,
    pub minor: u16,
}

/// Idempotency key of every API operation. Defined in `forge_core::ids` (with the other
/// ULID newtypes) and re-exported here; its `specta::Type` comes from forge-core's
/// `specta` feature.
pub use forge_core::ids::RequestId;

/// All types exported to TypeScript. Add new contract types here.
pub fn type_collection() -> Types {
    Types::default()
        .register::<ApiVersion>()
        .register::<RequestId>()
}

/// Renders the TypeScript bindings for [`type_collection`].
///
/// Output is deterministic (types sorted by name). No integer wider than 32 bits is
/// used by the contracts yet, so no BigInt policy is required.
pub fn export_typescript() -> anyhow::Result<String> {
    let ts = specta_typescript::Typescript::default()
        .export(&type_collection(), specta_serde::Format)
        .map_err(|e| anyhow::anyhow!("specta export failed: {e}"))?;
    Ok(ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typescript_contains_contract_types() {
        let ts = export_typescript().unwrap();
        assert!(ts.contains("ApiVersion"), "{ts}");
        assert!(ts.contains("RequestId"), "{ts}");
        assert!(ts.contains("major: number"), "{ts}");
    }

    #[test]
    fn export_is_deterministic() {
        assert_eq!(export_typescript().unwrap(), export_typescript().unwrap());
    }

    #[test]
    fn request_id_is_a_ulid_string_on_the_wire() {
        let id = RequestId::new();
        let json = serde_json::to_string(&id).unwrap();
        let text: String = serde_json::from_str(&json).unwrap();
        assert!(text.parse::<ulid::Ulid>().is_ok());
        assert_eq!(serde_json::from_str::<RequestId>(&json).unwrap(), id);
    }

    #[test]
    fn request_id_exports_as_string() {
        let ts = export_typescript().unwrap();
        assert!(ts.contains("export type RequestId = string;"), "{ts}");
    }
}
