//! Trust of the local CA (docs/06 §6.1, docs/08 §3, docs/11 §3).
//!
//! * The OS side is [`forge_core::platform::TrustStore`]: `CurrentUser\Root` without
//!   elevation (Windows shows its confirmation dialog once per change), `LocalMachine\Root`
//!   through `forgebox-helper` (`trust.add_machine_root {thumbprint}`).
//! * [`TrustLedger`] (`certs/ca/trust.json`) remembers every root Forgebox added, by
//!   thumbprint and scope, so a rotation whose removal of the old CA was declined, and the
//!   uninstaller, can still remove everything Forgebox put there (docs/08 §3).
//! * Other trust stores are **instructions**, never automatic: Node.js
//!   (`NODE_EXTRA_CA_CERTS`, set by Forgebox only on the processes it manages), Firefox
//!   (`policies.json`), Python and Java ([`instructions`]).

use std::path::{Path, PathBuf};

use forge_core::Timestamp;
use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::paths::{ForgeRoot, write_atomic};
use forge_core::platform::{CertThumbprint, TrustScope};
use serde::{Deserialize, Serialize};

use crate::ca::CA_CERT_FILE;
use crate::messages;

/// `certs/ca/trust.json`.
pub const LEDGER_FILE: &str = "trust.json";
/// Version of the ledger file.
pub const LEDGER_VERSION: u32 = 1;
/// Environment variable read by Node.js for extra roots.
pub const NODE_EXTRA_CA_CERTS: &str = "NODE_EXTRA_CA_CERTS";

/// One root Forgebox added to an OS store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustEntry {
    pub thumbprint_sha1: CertThumbprint,
    pub scope: TrustScope,
    pub subject: String,
    pub not_after: Timestamp,
    pub added_at: Timestamp,
}

/// Every root Forgebox added (`certs/ca/trust.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustLedger {
    pub v: u32,
    pub entries: Vec<TrustEntry>,
}

impl Default for TrustLedger {
    fn default() -> Self {
        Self {
            v: LEDGER_VERSION,
            entries: Vec::new(),
        }
    }
}

impl TrustLedger {
    /// Reads `<dir>/trust.json`; missing is an empty ledger. Blocking.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(LEDGER_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ForgeError::io_at("reading", &path, e)),
        };
        let ledger: Self = serde_json::from_str(&text)
            .map_err(|e| ForgeError::ConfigInvalid(format!("{}: {e}", path.display())))?;
        if ledger.v != LEDGER_VERSION {
            return Err(ForgeError::ConfigInvalid(format!(
                "{}: unsupported version {}",
                path.display(),
                ledger.v
            )));
        }
        Ok(ledger)
    }

    /// Writes `<dir>/trust.json` atomically (pretty JSON, LF). Blocking.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).at("creating", dir)?;
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|e| ForgeError::Internal(format!("serializing the trust ledger: {e}")))?;
        text.push('\n');
        write_atomic(&dir.join(LEDGER_FILE), text.as_bytes())
    }

    /// Records `entry`, replacing the entry of the same thumbprint and scope.
    pub fn record(&mut self, entry: TrustEntry) {
        self.forget(&entry.thumbprint_sha1, entry.scope);
        self.entries.push(entry);
    }

    /// Drops the entry of `thumbprint` in `scope`; whether there was one.
    pub fn forget(&mut self, thumbprint: &CertThumbprint, scope: TrustScope) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| !(e.thumbprint_sha1 == *thumbprint && e.scope == scope));
        before != self.entries.len()
    }

    /// Entries of `scope`.
    pub fn in_scope(&self, scope: TrustScope) -> impl Iterator<Item = &TrustEntry> {
        self.entries.iter().filter(move |e| e.scope == scope)
    }
}

/// `NODE_EXTRA_CA_CERTS` and the CA file of `root`: the process manager adds it to the
/// environment of the Node processes it starts (docs/06 §6.1).
pub fn node_extra_ca_certs(root: &ForgeRoot) -> (&'static str, PathBuf) {
    (NODE_EXTRA_CA_CERTS, root.certs_ca().join(CA_CERT_FILE))
}

/// `policies.json` that makes Firefox read the roots of the Windows store
/// (`security.enterprise_roots.enabled`).
pub fn firefox_policies_json() -> String {
    "{\n  \"policies\": {\n    \"Certificates\": {\n      \"ImportEnterpriseRoots\": true\n    }\n  }\n}\n"
        .to_owned()
}

/// How to make another program trust the CA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustInstruction {
    /// `node`, `firefox`, `python`, `java`.
    pub target: &'static str,
    pub title: &'static str,
    /// pt-BR, in order.
    pub steps: Vec<&'static str>,
    /// What to paste (environment variable, file content, command).
    pub snippet: String,
}

/// Instructions for the programs that do not read the Windows store (docs/06 §6.1).
pub fn instructions(root: &ForgeRoot) -> Vec<TrustInstruction> {
    let ca = root.certs_ca().join(CA_CERT_FILE);
    let ca = ca.display();
    vec![
        TrustInstruction {
            target: "node",
            title: messages::TRUST_NODE_TITLE,
            steps: messages::TRUST_NODE_STEPS.to_vec(),
            snippet: format!("setx {NODE_EXTRA_CA_CERTS} \"{ca}\""),
        },
        TrustInstruction {
            target: "firefox",
            title: messages::TRUST_FIREFOX_TITLE,
            steps: messages::TRUST_FIREFOX_STEPS.to_vec(),
            snippet: firefox_policies_json(),
        },
        TrustInstruction {
            target: "python",
            title: messages::TRUST_PYTHON_TITLE,
            steps: messages::TRUST_PYTHON_STEPS.to_vec(),
            snippet: format!("setx REQUESTS_CA_BUNDLE \"{ca}\""),
        },
        TrustInstruction {
            target: "java",
            title: messages::TRUST_JAVA_TITLE,
            steps: messages::TRUST_JAVA_STEPS.to_vec(),
            snippet: format!(
                "keytool -importcert -noprompt -alias forgebox-local-ca -cacerts -file \"{ca}\""
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(byte: u8, scope: TrustScope) -> TrustEntry {
        TrustEntry {
            thumbprint_sha1: CertThumbprint::from_bytes([byte; 20]),
            scope,
            subject: "CN=Forgebox Local CA x 2026".to_owned(),
            not_after: "2036-10-27T12:00:00Z".parse().unwrap(),
            added_at: "2026-10-27T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn ledger_round_trips_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            TrustLedger::load(dir.path()).unwrap(),
            TrustLedger::default()
        );
        let mut ledger = TrustLedger::default();
        ledger.record(entry(1, TrustScope::CurrentUser));
        ledger.record(entry(1, TrustScope::LocalMachine));
        ledger.record(entry(1, TrustScope::CurrentUser));
        assert_eq!(ledger.entries.len(), 2);
        ledger.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(LEDGER_FILE)).unwrap();
        assert!(text.contains("\"scope\": \"current_user\""), "{text}");
        assert!(!text.contains('\r'));
        let back = TrustLedger::load(dir.path()).unwrap();
        assert_eq!(back, ledger);
        assert_eq!(back.in_scope(TrustScope::LocalMachine).count(), 1);
        let mut back = back;
        assert!(back.forget(
            &CertThumbprint::from_bytes([1; 20]),
            TrustScope::CurrentUser
        ));
        assert!(!back.forget(
            &CertThumbprint::from_bytes([2; 20]),
            TrustScope::CurrentUser
        ));
        std::fs::write(dir.path().join(LEDGER_FILE), "{\"v\": 9, \"entries\": []}").unwrap();
        assert_eq!(
            TrustLedger::load(dir.path()).unwrap_err().code(),
            "config_invalid"
        );
    }

    #[test]
    fn instructions_point_at_the_ca_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let (var, path) = node_extra_ca_certs(&root);
        assert_eq!(var, "NODE_EXTRA_CA_CERTS");
        assert!(path.ends_with("certs/ca/forgebox-ca.crt"));
        let all = instructions(&root);
        let targets: Vec<_> = all.iter().map(|i| i.target).collect();
        assert_eq!(targets, ["node", "firefox", "python", "java"]);
        assert!(all[0].snippet.contains("forgebox-ca.crt"));
        let policies: serde_json::Value = serde_json::from_str(&firefox_policies_json()).unwrap();
        assert_eq!(
            policies["policies"]["Certificates"]["ImportEnterpriseRoots"],
            true
        );
    }
}
