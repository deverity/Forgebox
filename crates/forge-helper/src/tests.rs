//! Unit tests of the helper with a fake system ([`FakeSys`]): every operation planned
//! (dry run) and executed, refusals before any change, link and policy tampering.
//! Nothing here touches the machine; the binary itself is exercised by `tests/cli.rs`.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::helper::{
    HelperOp, HelperRequest, HelperResponse, HostsEntry, request_path, response_path,
};
use forge_core::ids::RequestId;
use forge_core::platform::{
    CaseSensitiveFs, CaseSensitivitySupport, CertificateInfo, CodeSign, CodeSignInfo, CodeSignKind,
    FirewallRuleSpec, NrptRule, NrptSource, PrivilegedOps, StagedFile,
};
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};

use super::*;

// ---------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct State {
    rules: Vec<NrptRule>,
    machine_root: Vec<CertificateInfo>,
    firewall: Vec<FirewallRuleSpec>,
    defender: BTreeSet<PathBuf>,
    installed: Vec<PathBuf>,
    /// Every changing call, in order.
    changes: Vec<String>,
    flushes: usize,
    next_rule: usize,
}

#[derive(Debug)]
struct FakeSys {
    elevated: bool,
    hosts: PathBuf,
    staging: PathBuf,
    installer_exit: i32,
    state: Mutex<State>,
}

impl FakeSys {
    fn new(dir: &Path) -> Self {
        let staging = dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        Self {
            elevated: true,
            hosts: dir.join("hosts"),
            staging,
            installer_exit: 0,
            state: Mutex::new(State::default()),
        }
    }

    fn changes(&self) -> Vec<String> {
        self.state.lock().unwrap().changes.clone()
    }

    fn change(&self, what: String) {
        self.state.lock().unwrap().changes.push(what);
    }
}

fn rule(id: &str, ns: &str, server: &str, comment: Option<&str>, source: NrptSource) -> NrptRule {
    NrptRule {
        id: id.into(),
        namespaces: vec![ns.into()],
        nameservers: vec![server.parse().unwrap()],
        source,
        comment: comment.map(str::to_owned),
    }
}

impl PrivilegedOps for FakeSys {
    fn is_elevated(&self) -> Result<bool> {
        Ok(self.elevated)
    }
    fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
        Ok(self.state.lock().unwrap().rules.clone())
    }
    fn nrpt_add(&self, namespace: &str, nameserver: IpAddr, comment: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.next_rule += 1;
        let id = format!("{{new-{}}}", s.next_rule);
        s.rules.push(rule(
            &id,
            namespace,
            &nameserver.to_string(),
            Some(comment),
            NrptSource::Local,
        ));
        s.changes
            .push(format!("nrpt.add {namespace} {nameserver} {comment}"));
        Ok(())
    }
    fn nrpt_remove(&self, rule_id: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.rules.retain(|r| r.id != rule_id);
        s.changes.push(format!("nrpt.remove {rule_id}"));
        Ok(())
    }
    fn hosts_path(&self) -> PathBuf {
        self.hosts.clone()
    }
    fn replace_system_file(&self, path: &Path, contents: &[u8]) -> Result<()> {
        std::fs::write(path, contents).unwrap();
        self.change(format!(
            "replace {}",
            path.file_name().unwrap().to_string_lossy()
        ));
        Ok(())
    }
    fn flush_dns_cache(&self) -> Result<()> {
        self.state.lock().unwrap().flushes += 1;
        Ok(())
    }
    fn certificate_info(&self, bytes: &[u8]) -> Result<CertificateInfo> {
        // Read-only parsing of the real platform.
        forge_core::platform::privileged_ops().certificate_info(bytes)
    }
    fn machine_root_find(&self, thumbprint: &str) -> Result<Option<CertificateInfo>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .machine_root
            .iter()
            .find(|c| c.thumbprint.eq_ignore_ascii_case(thumbprint))
            .cloned())
    }
    fn machine_root_add(&self, der: &[u8]) -> Result<()> {
        let info = self.certificate_info(der)?;
        let mut s = self.state.lock().unwrap();
        s.changes.push(format!("root.add {}", info.thumbprint));
        s.machine_root.push(info);
        Ok(())
    }
    fn machine_root_remove(&self, thumbprint: &str) -> Result<bool> {
        let mut s = self.state.lock().unwrap();
        let before = s.machine_root.len();
        s.machine_root
            .retain(|c| !c.thumbprint.eq_ignore_ascii_case(thumbprint));
        s.changes.push(format!("root.remove {thumbprint}"));
        Ok(s.machine_root.len() != before)
    }
    fn firewall_rule_count(&self, name: &str) -> Result<usize> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .firewall
            .iter()
            .filter(|r| r.name == name)
            .count())
    }
    fn firewall_add(&self, rule: &FirewallRuleSpec) -> Result<usize> {
        let mut s = self.state.lock().unwrap();
        let before = s.firewall.len();
        s.firewall.retain(|r| r.name != rule.name);
        let replaced = before - s.firewall.len();
        s.firewall.push(rule.clone());
        s.changes
            .push(format!("firewall.add {} {}", rule.name, rule.port));
        Ok(replaced)
    }
    fn firewall_remove(&self, name: &str) -> Result<usize> {
        let mut s = self.state.lock().unwrap();
        let before = s.firewall.len();
        s.firewall.retain(|r| r.name != name);
        s.changes.push(format!("firewall.remove {name}"));
        Ok(before - s.firewall.len())
    }
    fn defender_add_exclusion(&self, path: &Path) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.defender.insert(path.to_path_buf());
        s.changes.push("defender.add".into());
        Ok(())
    }
    fn defender_remove_exclusion(&self, path: &Path) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.defender.remove(path);
        s.changes.push("defender.remove".into());
        Ok(())
    }
    fn stage_verified_copy(&self, source: &Path, sha256: &str, tag: &str) -> Result<StagedFile> {
        let bytes = std::fs::read(source).unwrap();
        let actual: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !actual.eq_ignore_ascii_case(sha256) {
            return Err(ForgeError::VerificationFailed(format!(
                "sha256 is {actual}"
            )));
        }
        let dir = self.staging.join(tag);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(source.file_name().unwrap());
        std::fs::write(&path, bytes).unwrap();
        self.change(format!("stage {tag}"));
        Ok(StagedFile::new(dir, path))
    }
    fn run_installer(&self, exe: &Path, args: &[&str], _: Duration) -> Result<i32> {
        let mut s = self.state.lock().unwrap();
        s.installed.push(exe.to_path_buf());
        s.changes.push(format!("run {}", args.join(" ")));
        Ok(self.installer_exit)
    }
}

#[derive(Debug)]
struct FakeSign(CodeSignInfo);

impl CodeSign for FakeSign {
    fn verify(&self, path: &Path) -> Result<CodeSignInfo> {
        assert!(path.is_file(), "{} verified while missing", path.display());
        Ok(self.0.clone())
    }
}

fn microsoft() -> CodeSignInfo {
    CodeSignInfo {
        signed: true,
        trusted: true,
        publisher: Some("Microsoft Corporation".into()),
        kind: Some(CodeSignKind::Embedded),
        status: None,
    }
}

#[derive(Debug, Default)]
struct FakeCase(Mutex<BTreeSet<PathBuf>>);

impl CaseSensitiveFs for FakeCase {
    fn support(&self, _: &Path) -> CaseSensitivitySupport {
        CaseSensitivitySupport::Supported
    }
    fn get(&self, dir: &Path) -> Result<bool> {
        Ok(self.0.lock().unwrap().contains(dir))
    }
    fn set(&self, dir: &Path, enabled: bool) -> Result<()> {
        assert!(enabled);
        self.0.lock().unwrap().insert(dir.to_path_buf());
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------

const INSTALLER: &[u8] = b"MZ fake vc_redist";

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    sys: Arc<FakeSys>,
    case: Arc<FakeCase>,
    sign: CodeSignInfo,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Forge box");
        for sub in [
            "bin/nginx/1.27.4",
            "tmp/downloads",
            "www/loja",
            "certs/ca",
            "etc",
            "state/locks",
            "data",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(root.join("bin/nginx/1.27.4/nginx.exe"), b"MZ").unwrap();
        std::fs::write(root.join("tmp/downloads/vc_redist.x64.exe"), INSTALLER).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let sys = Arc::new(FakeSys::new(dir.path()));
        Self {
            _dir: dir,
            root,
            outside,
            sys,
            case: Arc::new(FakeCase::default()),
            sign: microsoft(),
        }
    }

    fn helper(&self) -> Helper {
        Helper::new(
            &self.root,
            self.sys.clone(),
            self.case.clone(),
            Arc::new(FakeSign(self.sign.clone())),
        )
    }

    fn request(&self, op: HelperOp) -> HelperRequest {
        HelperRequest::new(&self.root, op)
    }

    fn handle(&self, op: HelperOp, dry_run: bool) -> HelperResponse {
        let request = self.request(op);
        let (response, name) = self
            .helper()
            .handle(&request.to_json().unwrap(), None, dry_run);
        assert_eq!(name, Some(request.op.name()));
        assert_eq!(response.request_id, request.request_id);
        response
    }

    fn ok(&self, op: HelperOp, dry_run: bool) -> HelperResponse {
        let response = self.handle(op.clone(), dry_run);
        assert!(
            response.ok,
            "{op:?}: {} ({:?})",
            response.message, response.code
        );
        assert_eq!(response.details["dry_run"], dry_run, "{op:?}");
        response
    }

    fn refused(&self, op: HelperOp, dry_run: bool, code: &str) -> HelperResponse {
        let response = self.handle(op.clone(), dry_run);
        assert!(!response.ok, "{op:?} must be refused: {}", response.message);
        assert_eq!(
            response.code.as_deref(),
            Some(code),
            "{op:?}: {}",
            response.message
        );
        response
    }

    fn sha(&self) -> String {
        Sha256::digest(INSTALLER)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn all_ops(&self) -> Vec<HelperOp> {
        vec![
            HelperOp::NrptAdd {
                namespace: ".test".into(),
                nameserver: "127.0.0.1".parse().unwrap(),
            },
            HelperOp::NrptRemove {
                namespace: ".test".into(),
            },
            HelperOp::HostsSetBlock {
                entries: vec![HostsEntry::new("127.0.0.1".parse().unwrap(), "loja.test")],
            },
            HelperOp::TrustRemoveMachineRoot {
                thumbprint: "ab".repeat(20),
            },
            HelperOp::FirewallAdd {
                name: "Forgebox-nginx".into(),
                port: 443,
                exe: self.root.join("bin/nginx/1.27.4/nginx.exe"),
            },
            HelperOp::FirewallRemove {
                name: "Forgebox-nginx".into(),
            },
            HelperOp::DefenderExclude {
                path: self.root.clone(),
            },
            HelperOp::DefenderRemove {
                path: self.root.clone(),
            },
            HelperOp::VcredistInstall {
                path: self.root.join("tmp/downloads/vc_redist.x64.exe"),
                sha256: self.sha(),
            },
            HelperOp::FsSetCaseSensitive {
                path: self.root.join("www/loja"),
            },
        ]
    }
}

/// A test CA (`cn`, Name Constraints `dns`), PEM, and its parsed thumbprint.
fn test_ca(cn: &str, dns: &[&str]) -> (String, String) {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, GeneralSubtree, IsCa, KeyPair, NameConstraints,
    };
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, cn);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    if !dns.is_empty() {
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: dns
                .iter()
                .map(|d| GeneralSubtree::DnsName((*d).to_owned()))
                .chain([GeneralSubtree::IpAddress(
                    rcgen::CidrSubnet::from_v4_prefix([127, 0, 0, 0], 8),
                )])
                .collect(),
            excluded_subtrees: vec![],
        });
    }
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let pem = cert.pem();
    let info = forge_core::platform::privileged_ops()
        .certificate_info(pem.as_bytes())
        .unwrap();
    (pem, info.thumbprint)
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

#[test]
fn dry_runs_plan_every_operation_without_changing_anything() {
    let fx = Fx::new();
    for op in fx.all_ops() {
        let response = fx.ok(op.clone(), true);
        assert!(
            response.message.starts_with("dry run")
                || response.message.contains("no Forgebox")
                || response.message.contains("not in"),
            "{op:?}: {}",
            response.message
        );
    }
    assert!(fx.sys.changes().is_empty(), "{:?}", fx.sys.changes());
    assert_eq!(fx.sys.state.lock().unwrap().flushes, 0);
    assert!(!fx.sys.hosts.exists());
    // Dry runs never need elevation.
    let mut sys = FakeSys::new(&fx.outside);
    sys.elevated = false;
    let helper = Helper::new(
        &fx.root,
        Arc::new(sys),
        fx.case.clone(),
        Arc::new(FakeSign(microsoft())),
    );
    let req = fx.request(fx.all_ops()[0].clone());
    assert!(helper.handle(&req.to_json().unwrap(), None, true).0.ok);
}

#[test]
fn every_operation_executes_when_elevated() {
    let fx = Fx::new();
    for op in fx.all_ops() {
        fx.ok(op, false);
    }
    let changes = fx.sys.changes();
    let root = plain_path(&std::fs::canonicalize(&fx.root).unwrap());
    assert_eq!(
        changes[..7],
        [
            "nrpt.add .test 127.0.0.1 Forgebox",
            "nrpt.remove {new-1}",
            "replace hosts",
            "firewall.add Forgebox-nginx 443",
            "firewall.remove Forgebox-nginx",
            "defender.add",
            "defender.remove",
        ]
    );
    assert!(changes[7].starts_with("stage "), "{changes:?}");
    assert_eq!(changes[8], "run /install /quiet /norestart");
    assert_eq!(changes.len(), 9, "{changes:?}");
    let state = fx.sys.state.lock().unwrap();
    assert_eq!(state.flushes, 3, "after nrpt.add, nrpt.remove and hosts");
    assert!(state.defender.is_empty());
    assert_eq!(state.installed.len(), 1);
    assert!(
        !state.installed[0].exists(),
        "staging directory removed after the run"
    );
    drop(state);
    assert!(
        fx.case
            .0
            .lock()
            .unwrap()
            .contains(&root.join("www").join("loja")),
        "canonical path flagged"
    );
}

#[test]
fn nothing_changes_without_elevation() {
    let fx = Fx::new();
    let mut sys = FakeSys::new(&fx.outside);
    sys.elevated = false;
    let sys = Arc::new(sys);
    let helper = Helper::new(
        &fx.root,
        sys.clone(),
        fx.case.clone(),
        Arc::new(FakeSign(microsoft())),
    );
    for op in fx.all_ops() {
        let req = fx.request(op.clone());
        let (resp, _) = helper.handle(&req.to_json().unwrap(), None, false);
        assert_eq!(resp.code.as_deref(), Some("needs_elevation"), "{op:?}");
    }
    assert!(sys.changes().is_empty());
}

#[test]
fn malformed_unknown_and_foreign_requests_are_refused() {
    let fx = Fx::new();
    let helper = fx.helper();
    let id = RequestId::new();

    let (resp, op) = helper.handle("{not json", Some(id), true);
    assert_eq!(
        (resp.code.as_deref(), resp.request_id, op),
        (Some("invalid_input"), id, None)
    );

    let unknown = serde_json::json!({
        "v": 1, "request_id": id.to_string(), "root": fx.root,
        "op": {"name": "shell.run", "params": {"command": "cmd /c whoami"}}
    });
    let (resp, _) = helper.handle(&unknown.to_string(), None, false);
    assert_eq!(resp.code.as_deref(), Some("forbidden"));
    assert_eq!(resp.request_id, id, "id read from the broken request");
    assert!(resp.message.contains("allowlist"), "{}", resp.message);

    let extra = serde_json::json!({
        "v": 1, "request_id": id.to_string(), "root": fx.root,
        "op": {"name": "firewall.remove", "params": {"name": "Forgebox-x", "force": true}}
    });
    assert_eq!(
        helper
            .handle(&extra.to_string(), None, false)
            .0
            .code
            .as_deref(),
        Some("invalid_input")
    );

    // The id inside must match the file it came from.
    let req = fx.request(HelperOp::FirewallRemove {
        name: "Forgebox-x".into(),
    });
    let (resp, _) = helper.handle(&req.to_json().unwrap(), Some(id), false);
    assert_eq!(
        (resp.code.as_deref(), resp.request_id),
        (Some("forbidden"), id)
    );

    // Another root: refused even if it is a valid Forgebox root.
    let other = HelperRequest::new(
        &fx.outside,
        HelperOp::FirewallRemove {
            name: "Forgebox-x".into(),
        },
    );
    let (resp, _) = helper.handle(&other.to_json().unwrap(), None, false);
    assert_eq!(resp.code.as_deref(), Some("forbidden"), "{}", resp.message);
    let missing = HelperRequest::new(
        fx.outside.join("nope"),
        HelperOp::FirewallRemove {
            name: "Forgebox-x".into(),
        },
    );
    assert_eq!(
        helper
            .handle(&missing.to_json().unwrap(), None, true)
            .0
            .code
            .as_deref(),
        Some("forbidden")
    );
    assert!(fx.sys.changes().is_empty());
}

#[test]
fn parameters_are_validated_again_by_the_helper() {
    let fx = Fx::new();
    let refusals: Vec<(HelperOp, &str)> = vec![
        (
            HelperOp::NrptAdd {
                namespace: ".com".into(),
                nameserver: "127.0.0.1".parse().unwrap(),
            },
            "forbidden",
        ),
        (
            HelperOp::NrptAdd {
                namespace: ".test".into(),
                nameserver: "10.0.0.1".parse().unwrap(),
            },
            "forbidden",
        ),
        (
            HelperOp::NrptRemove {
                namespace: "test".into(),
            },
            "invalid_input",
        ),
        (
            HelperOp::HostsSetBlock {
                entries: vec![HostsEntry::new("127.0.0.1".parse().unwrap(), "lo ja.test")],
            },
            "invalid_input",
        ),
        (
            HelperOp::HostsSetBlock {
                entries: vec![HostsEntry::new("10.1.1.1".parse().unwrap(), "loja.test")],
            },
            "forbidden",
        ),
        (
            HelperOp::HostsSetBlock {
                entries: vec![HostsEntry::new("127.0.0.1".parse().unwrap(), "google.com")],
            },
            "forbidden",
        ),
        (
            HelperOp::TrustAddMachineRoot {
                thumbprint: "xyz".into(),
            },
            "invalid_input",
        ),
        (
            HelperOp::TrustRemoveMachineRoot {
                thumbprint: "a".repeat(39),
            },
            "invalid_input",
        ),
        (
            HelperOp::FirewallAdd {
                name: "nginx".into(),
                port: 80,
                exe: fx.root.join("bin/nginx/1.27.4/nginx.exe"),
            },
            "forbidden",
        ),
        (
            HelperOp::FirewallAdd {
                name: "Forgebox-x".into(),
                port: 80,
                exe: fx.root.join("data/x.exe"),
            },
            "forbidden",
        ),
        (
            HelperOp::FirewallAdd {
                name: "Forgebox-x".into(),
                port: 0,
                exe: fx.root.join("bin/nginx/1.27.4/nginx.exe"),
            },
            "invalid_input",
        ),
        (
            HelperOp::FirewallRemove {
                name: "Forgebox-a b".into(),
            },
            "invalid_input",
        ),
        (
            HelperOp::DefenderExclude {
                path: fx.root.join("www"),
            },
            "forbidden",
        ),
        (
            HelperOp::DefenderRemove {
                path: fx.outside.clone(),
            },
            "forbidden",
        ),
        (
            HelperOp::VcredistInstall {
                path: fx.root.join("bin/vc_redist.x64.exe"),
                sha256: fx.sha(),
            },
            "forbidden",
        ),
        (
            HelperOp::VcredistInstall {
                path: fx.root.join("tmp/downloads/vc_redist.x64.exe"),
                sha256: "zz".into(),
            },
            "invalid_input",
        ),
        (
            HelperOp::FsSetCaseSensitive {
                path: fx.root.join("state"),
            },
            "forbidden",
        ),
        (
            HelperOp::FsSetCaseSensitive {
                path: fx.root.clone(),
            },
            "forbidden",
        ),
        (
            HelperOp::FsSetCaseSensitive {
                path: fx.outside.clone(),
            },
            "forbidden",
        ),
    ];
    for (op, code) in refusals {
        fx.refused(op.clone(), true, code);
        fx.refused(op, false, code);
    }
    assert!(fx.sys.changes().is_empty());
}

#[test]
fn the_policy_comes_from_the_helpers_root_and_local_use_suffixes() {
    let fx = Fx::new();
    // A tampered forgebox.toml cannot route a public TLD.
    std::fs::write(fx.root.join("etc/forgebox.toml"), "tld = \".com\"\n").unwrap();
    fx.refused(
        HelperOp::NrptAdd {
            namespace: ".com".into(),
            nameserver: "127.0.0.1".parse().unwrap(),
        },
        false,
        "forbidden",
    );
    // ...and once configured, an extra local TLD works; .test is no longer configured.
    std::fs::write(
        fx.root.join("etc/forgebox.toml"),
        "[dns]\nextra_tlds = [\".lan\"]\n",
    )
    .unwrap();
    fx.ok(
        HelperOp::NrptAdd {
            namespace: ".lan".into(),
            nameserver: "127.0.0.53".parse().unwrap(),
        },
        false,
    );
    std::fs::write(fx.root.join("etc/forgebox.toml"), "tld = \"broken").unwrap();
    fx.refused(
        HelperOp::NrptRemove {
            namespace: ".test".into(),
        },
        true,
        "config_invalid",
    );
    // Parked directories outside the root, from the same file.
    let parked = fx.outside.join("clientes");
    std::fs::create_dir_all(parked.join("loja")).unwrap();
    std::fs::write(
        fx.root.join("etc/forgebox.toml"),
        format!(
            "parked_dirs = [\"www\", {:?}]\n",
            parked.display().to_string()
        ),
    )
    .unwrap();
    fx.ok(
        HelperOp::FsSetCaseSensitive {
            path: parked.join("loja"),
        },
        false,
    );
}

#[cfg(windows)]
#[test]
fn junctions_cannot_lead_out_of_the_allowed_directories() {
    let fx = Fx::new();
    // bin/evil -> a directory outside the root holding an exe.
    std::fs::write(fx.outside.join("cmd.exe"), b"MZ").unwrap();
    junction::create(&fx.outside, fx.root.join("bin").join("evil")).unwrap();
    fx.refused(
        HelperOp::FirewallAdd {
            name: "Forgebox-evil".into(),
            port: 80,
            exe: fx.root.join("bin/evil/cmd.exe"),
        },
        true,
        "forbidden",
    );
    // tmp/downloads/x -> outside: the installer would come from elsewhere.
    std::fs::write(fx.outside.join("vc_redist.x64.exe"), INSTALLER).unwrap();
    junction::create(&fx.outside, fx.root.join("tmp/downloads/x")).unwrap();
    fx.refused(
        HelperOp::VcredistInstall {
            path: fx.root.join("tmp/downloads/x/vc_redist.x64.exe"),
            sha256: fx.sha(),
        },
        false,
        "forbidden",
    );
    // www/link -> state/: flagging it would flag state/.
    junction::create(fx.root.join("state"), fx.root.join("www/link")).unwrap();
    fx.refused(
        HelperOp::FsSetCaseSensitive {
            path: fx.root.join("www/link"),
        },
        false,
        "forbidden",
    );
    // Missing paths cannot be canonicalized.
    fx.refused(
        HelperOp::FirewallAdd {
            name: "Forgebox-x".into(),
            port: 80,
            exe: fx.root.join("bin/nginx/1.27.4/missing.exe"),
        },
        true,
        "not_found",
    );
    assert!(fx.sys.changes().is_empty());
}

#[test]
fn nrpt_rules_are_replaced_only_when_they_are_ours() {
    let fx = Fx::new();
    {
        let mut s = fx.sys.state.lock().unwrap();
        s.rules = vec![
            rule(
                "{old}",
                ".test",
                "127.0.0.53",
                Some("Forgebox"),
                NrptSource::Local,
            ),
            rule(
                "{vpn}",
                ".test",
                "10.8.0.1",
                Some("Corp VPN"),
                NrptSource::Local,
            ),
            rule(
                "{other}",
                ".lan",
                "127.0.0.1",
                Some("Forgebox"),
                NrptSource::Local,
            ),
        ];
    }
    let add = HelperOp::NrptAdd {
        namespace: ".test".into(),
        nameserver: "127.0.0.1".parse().unwrap(),
    };
    let plan = fx.ok(add.clone(), true);
    assert_eq!(plan.details["replace_rules"], serde_json::json!(["{old}"]));
    assert_eq!(plan.details["foreign_rules"], serde_json::json!(["{vpn}"]));
    assert_eq!(plan.details["effective"], true);

    let done = fx.ok(add.clone(), false);
    assert_eq!(done.details["rule_id"], "{new-1}");
    assert_eq!(
        fx.sys.changes(),
        vec!["nrpt.remove {old}", "nrpt.add .test 127.0.0.1 Forgebox"]
    );
    // Again: already present, nothing changes.
    let again = fx.ok(add, false);
    assert_eq!(again.details["already_present"], true);
    assert_eq!(fx.sys.changes().len(), 2);

    // Group Policy rules make local ones ineffective: reported for the daemon's fallback.
    fx.sys.state.lock().unwrap().rules.push(rule(
        "{gpo}",
        ".corp",
        "10.0.0.1",
        None,
        NrptSource::GroupPolicy,
    ));
    let plan = fx.ok(
        HelperOp::NrptAdd {
            namespace: ".test".into(),
            nameserver: "127.0.0.1".parse().unwrap(),
        },
        true,
    );
    assert_eq!(
        (
            plan.details["group_policy_rules"].clone(),
            plan.details["effective"].clone()
        ),
        (1.into(), false.into())
    );

    // Remove: only ours, the VPN rule stays.
    fx.ok(
        HelperOp::NrptRemove {
            namespace: ".test".into(),
        },
        false,
    );
    let ids: Vec<String> = fx
        .sys
        .state
        .lock()
        .unwrap()
        .rules
        .iter()
        .map(|r| r.id.clone())
        .collect();
    assert_eq!(ids, vec!["{vpn}", "{other}", "{gpo}"]);
    let none = fx.ok(
        HelperOp::NrptRemove {
            namespace: ".test".into(),
        },
        false,
    );
    assert!(none.message.contains("no Forgebox"), "{}", none.message);
}

#[test]
fn the_hosts_block_keeps_everything_else_byte_for_byte() {
    let fx = Fx::new();
    let original = b"# Configura\xE7\xE3o\r\n127.0.0.1 localhost\r\n10.0.0.9 nas.lan\r\n".to_vec();
    std::fs::write(&fx.sys.hosts, &original).unwrap();
    let entries = vec![
        HostsEntry::new("127.0.0.1".parse().unwrap(), "loja.test"),
        HostsEntry::new("::1".parse().unwrap(), "loja.test"),
    ];
    let plan = fx.ok(
        HelperOp::HostsSetBlock {
            entries: entries.clone(),
        },
        true,
    );
    assert_eq!(plan.details["action"], "add");
    assert_eq!(std::fs::read(&fx.sys.hosts).unwrap(), original, "dry run");

    fx.ok(
        HelperOp::HostsSetBlock {
            entries: entries.clone(),
        },
        false,
    );
    let written = std::fs::read(&fx.sys.hosts).unwrap();
    assert!(written.starts_with(&original));
    assert!(
        String::from_utf8_lossy(&written).contains("::1 loja.test\r\n# forgebox:end hosts\r\n")
    );

    let same = fx.ok(HelperOp::HostsSetBlock { entries }, false);
    assert_eq!(same.details["action"], "unchanged");
    let removed = fx.ok(HelperOp::HostsSetBlock { entries: vec![] }, false);
    assert_eq!(removed.details["action"], "remove");
    assert_eq!(std::fs::read(&fx.sys.hosts).unwrap(), original);
    assert_eq!(fx.sys.changes(), vec!["replace hosts", "replace hosts"]);
}

#[cfg(windows)]
#[test]
fn only_a_constrained_forgebox_ca_is_trusted_machine_wide() {
    let fx = Fx::new();
    let (good_pem, good) = test_ca("Forgebox Local CA test 2026", &["test", "localhost"]);
    let (open_pem, open) = test_ca("Forgebox Local CA open 2026", &[]);
    let (public_pem, public) = test_ca("Forgebox Local CA dev 2026", &["test", "dev"]);
    let (evil_pem, evil) = test_ca("Evil Root", &["test"]);
    let ca = fx.root.join("certs/ca");
    std::fs::write(ca.join("forgebox-ca.crt"), &good_pem).unwrap();
    std::fs::write(ca.join("open.pem"), &open_pem).unwrap();
    std::fs::write(ca.join("public.cer"), &public_pem).unwrap();
    std::fs::write(ca.join("evil.crt"), &evil_pem).unwrap();
    std::fs::write(ca.join("garbage.crt"), b"not a certificate").unwrap();

    let plan = fx.ok(
        HelperOp::TrustAddMachineRoot {
            thumbprint: good.to_uppercase(),
        },
        true,
    );
    assert_eq!(plan.details["already_trusted"], false);
    assert_eq!(
        plan.details["name_constraints"]["permitted_dns"],
        serde_json::json!(["test", "localhost"])
    );
    fx.ok(
        HelperOp::TrustAddMachineRoot {
            thumbprint: good.clone(),
        },
        false,
    );
    assert_eq!(fx.sys.changes(), vec![format!("root.add {good}")]);
    let again = fx.ok(
        HelperOp::TrustAddMachineRoot {
            thumbprint: good.clone(),
        },
        false,
    );
    assert_eq!(again.details["already_trusted"], true);
    assert_eq!(fx.sys.changes().len(), 1);

    for thumbprint in [open, public, evil] {
        fx.refused(
            HelperOp::TrustAddMachineRoot { thumbprint },
            false,
            "forbidden",
        );
    }
    fx.refused(
        HelperOp::TrustAddMachineRoot {
            thumbprint: "0".repeat(40),
        },
        false,
        "not_found",
    );

    // Removal: ours yes, someone else's no, absent is fine.
    fx.ok(
        HelperOp::TrustRemoveMachineRoot {
            thumbprint: good.clone(),
        },
        false,
    );
    assert!(fx.sys.machine_root_find(&good).unwrap().is_none());
    let microsoft_root = forge_core::platform::privileged_ops()
        .machine_root_find("8F43288AD272F3103B6FB1428485EA3014C0BCFE")
        .unwrap()
        .unwrap();
    fx.sys
        .state
        .lock()
        .unwrap()
        .machine_root
        .push(microsoft_root);
    fx.refused(
        HelperOp::TrustRemoveMachineRoot {
            thumbprint: "8F43288AD272F3103B6FB1428485EA3014C0BCFE".into(),
        },
        false,
        "forbidden",
    );
    let absent = fx.ok(
        HelperOp::TrustRemoveMachineRoot {
            thumbprint: "1".repeat(40),
        },
        false,
    );
    assert_eq!(absent.details["present"], false);
}

#[test]
fn the_vc_redist_installer_is_verified_before_running() {
    let fx = Fx::new();
    let path = fx.root.join("tmp/downloads/vc_redist.x64.exe");
    fx.refused(
        HelperOp::VcredistInstall {
            path: path.clone(),
            sha256: "0".repeat(64),
        },
        true,
        "verification_failed",
    );
    fx.refused(
        HelperOp::VcredistInstall {
            path: path.clone(),
            sha256: "0".repeat(64),
        },
        false,
        "verification_failed",
    );

    for (publisher, trusted) in [("Evil Corp", true), ("Microsoft Corporation", false)] {
        let mut fx2 = Fx::new();
        fx2.sign.publisher = Some(publisher.into());
        fx2.sign.trusted = trusted;
        let op = HelperOp::VcredistInstall {
            path: fx2.root.join("tmp/downloads/vc_redist.x64.exe"),
            sha256: fx2.sha(),
        };
        fx2.refused(op.clone(), true, "verification_failed");
        fx2.refused(op, false, "verification_failed");
        assert!(fx2.sys.state.lock().unwrap().installed.is_empty());
    }

    for (exit, ok, reboot) in [
        (0, true, false),
        (3010, true, true),
        (1638, true, false),
        (1603, false, false),
    ] {
        let mut fx3 = Fx::new();
        Arc::get_mut(&mut fx3.sys).unwrap().installer_exit = exit;
        let op = HelperOp::VcredistInstall {
            path: fx3.root.join("tmp/downloads/vc_redist.x64.exe"),
            sha256: fx3.sha().to_uppercase(),
        };
        let resp = fx3.handle(op, false);
        assert_eq!(resp.ok, ok, "exit {exit}: {}", resp.message);
        if ok {
            assert_eq!(resp.details["exit_code"], exit);
            assert_eq!(resp.details["reboot_required"], reboot);
        } else {
            assert_eq!(resp.code.as_deref(), Some("io"));
            assert!(resp.message.contains("1603"), "{}", resp.message);
        }
    }
}

#[test]
fn case_sensitivity_is_idempotent() {
    let fx = Fx::new();
    let op = HelperOp::FsSetCaseSensitive {
        path: fx.root.join("www/loja"),
    };
    fx.ok(op.clone(), false);
    let again = fx.ok(op, false);
    assert!(again.message.contains("already"), "{}", again.message);
    std::fs::write(fx.root.join("www/file.txt"), b"x").unwrap();
    fx.refused(
        HelperOp::FsSetCaseSensitive {
            path: fx.root.join("www/file.txt"),
        },
        false,
        "invalid_input",
    );
}

#[test]
fn run_uses_the_exchange_files_and_logs_everything() {
    let fx = Fx::new();
    let helper = fx.helper();
    let request = fx.request(HelperOp::FirewallRemove {
        name: "Forgebox-nginx".into(),
    });
    let req_path = request_path(&fx.root, request.request_id);
    let resp_path = response_path(&fx.root, request.request_id);
    std::fs::write(&req_path, request.to_json().unwrap()).unwrap();
    std::fs::write(&resp_path, "").unwrap();
    let args = RunArgs {
        request: Input::File(req_path.clone()),
        response: Some(resp_path.clone()),
        dry_run: false,
    };
    assert_eq!(run(&helper, &args), EXIT_OK);
    let response =
        HelperResponse::from_json(&std::fs::read_to_string(&resp_path).unwrap()).unwrap();
    assert!(response.ok);
    assert_eq!(response.request_id, request.request_id);

    // A refusal is answered too, with exit code 1.
    let bad = fx.request(HelperOp::FirewallRemove {
        name: "nginx".into(),
    });
    let bad_req = request_path(&fx.root, bad.request_id);
    let bad_resp = response_path(&fx.root, bad.request_id);
    std::fs::write(&bad_req, bad.to_json().unwrap()).unwrap();
    std::fs::write(&bad_resp, "").unwrap();
    let args = RunArgs {
        request: Input::File(bad_req),
        response: Some(bad_resp.clone()),
        dry_run: false,
    };
    assert_eq!(run(&helper, &args), EXIT_REFUSED);
    let refusal = HelperResponse::from_json(&std::fs::read_to_string(&bad_resp).unwrap()).unwrap();
    assert_eq!(refusal.code.as_deref(), Some("forbidden"));

    // Files outside the layout: usage error, nothing written.
    let stray = fx.outside.join("resp.json");
    std::fs::write(&stray, "").unwrap();
    let args = RunArgs {
        request: Input::File(req_path),
        response: Some(stray.clone()),
        dry_run: false,
    };
    assert_eq!(run(&helper, &args), EXIT_USAGE);
    assert_eq!(std::fs::read_to_string(&stray).unwrap(), "");

    // The response file is gone (daemon gave up): exit 3.
    let lost = fx.request(HelperOp::FirewallRemove {
        name: "Forgebox-x".into(),
    });
    let lost_req = request_path(&fx.root, lost.request_id);
    std::fs::write(&lost_req, lost.to_json().unwrap()).unwrap();
    let args = RunArgs {
        request: Input::File(lost_req),
        response: Some(response_path(&fx.root, lost.request_id)),
        dry_run: false,
    };
    assert_eq!(run(&helper, &args), EXIT_NO_RESPONSE);

    let log = std::fs::read_to_string(fx.root.join("logs").join(log::LOG_FILE)).unwrap();
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4, "{log}");
    assert_eq!(lines[0]["op"], "firewall.remove");
    assert_eq!(lines[0]["ok"], true);
    assert_eq!(lines[0]["request_id"], request.request_id.to_string());
    assert_eq!(lines[1]["code"], "forbidden");
    assert_eq!(lines[2]["ok"], false, "usage errors are logged too");
    assert_eq!(
        fx.sys.changes(),
        vec![
            "firewall.remove Forgebox-nginx",
            "firewall.remove Forgebox-x"
        ]
    );
}

#[cfg(windows)]
#[test]
fn no_log_no_action() {
    let fx = Fx::new();
    junction::create(&fx.outside, fx.root.join("logs")).unwrap();
    let request = fx.request(HelperOp::FirewallRemove {
        name: "Forgebox-nginx".into(),
    });
    let req_path = request_path(&fx.root, request.request_id);
    let resp_path = response_path(&fx.root, request.request_id);
    std::fs::write(&req_path, request.to_json().unwrap()).unwrap();
    std::fs::write(&resp_path, "").unwrap();
    let args = RunArgs {
        request: Input::File(req_path),
        response: Some(resp_path.clone()),
        dry_run: false,
    };
    assert_eq!(run(&fx.helper(), &args), EXIT_REFUSED);
    let response =
        HelperResponse::from_json(&std::fs::read_to_string(&resp_path).unwrap()).unwrap();
    assert_eq!(response.code.as_deref(), Some("forbidden"));
    assert!(
        response.message.contains("helper.log"),
        "{}",
        response.message
    );
    assert!(fx.sys.changes().is_empty());
    assert_eq!(std::fs::read_dir(&fx.outside).unwrap().count(), 0);
}

#[test]
fn banner_has_name_and_version() {
    assert_eq!(
        banner(),
        format!("forgebox-helper {}", env!("CARGO_PKG_VERSION"))
    );
}
