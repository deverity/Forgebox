//! The compiled `forgebox-helper.exe` end to end, **without elevation and without any UAC
//! prompt**: the binary is copied into a temporary Forgebox root (the helper only serves
//! the root it lives in) and started with `__COMPAT_LAYER=RunAsInvoker`, the Windows
//! application-compatibility switch that runs a `requireAdministrator` program with the
//! caller's token. Every operation is exercised with `--dry-run` (validation + plan, no
//! change); a real run must come back `needs_elevation` when the tests are not elevated.
#![cfg(windows)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use forge_core::helper::{
    HelperOp, HelperRequest, HelperResponse, HostsEntry, request_path, response_path,
};
use forge_core::ids::RequestId;
use serde_json::{Value, json};

const HELPER: &str = env!("CARGO_BIN_EXE_forgebox-helper");

struct Root {
    _dir: tempfile::TempDir,
    root: PathBuf,
    exe: PathBuf,
}

impl Root {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // A space in the root exercises quoting and path handling.
        let root = dir.path().join("Forge box");
        for sub in [
            "bin/nginx/1.27.4",
            "tmp/downloads",
            "www/loja",
            "certs/ca",
            "etc",
            "state/locks",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(root.join("bin/nginx/1.27.4/nginx.exe"), b"MZ").unwrap();
        let exe = root.join("forgebox-helper.exe");
        std::fs::copy(HELPER, &exe).unwrap();
        Self {
            _dir: dir,
            root,
            exe,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.env("__COMPAT_LAYER", "RunAsInvoker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .spawn()
            .expect("starting forgebox-helper");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// `--dry-run --request -` with `request`; exit code and parsed response.
    fn dry_run(&self, request: &str) -> (i32, HelperResponse) {
        let out = self.run(&["--dry-run", "--request", "-"], request);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let response = HelperResponse::from_json(&stdout).unwrap_or_else(|e| {
            panic!(
                "{e}\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap(), response)
    }

    fn dry_run_op(&self, op: HelperOp) -> (i32, HelperResponse) {
        self.dry_run(&HelperRequest::new(&self.root, op).to_json().unwrap())
    }

    fn log_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("logs/helper.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn entry(ip: &str, host: &str) -> HostsEntry {
    HostsEntry::new(ip.parse().unwrap(), host)
}

#[test]
fn the_binary_demands_elevation() {
    let root = Root::new();
    // Without the compatibility switch a non-elevated CreateProcess is refused by
    // Windows with ERROR_ELEVATION_REQUIRED (740): the manifest is in effect. (An
    // elevated test runner starts it normally.)
    let elevated = forge_core::platform::windows::process_is_elevated().unwrap();
    let result = Command::new(&root.exe).arg("--version").output();
    if elevated {
        assert!(result.unwrap().status.success());
    } else {
        assert_eq!(result.unwrap_err().raw_os_error(), Some(740));
    }
    let out = root.run(&["--version"], "");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("forgebox-helper {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn every_operation_dry_runs() {
    let root = Root::new();
    let ops = vec![
        HelperOp::NrptAdd {
            namespace: ".test".into(),
            nameserver: "127.0.0.1".parse().unwrap(),
        },
        HelperOp::NrptRemove {
            namespace: ".test".into(),
        },
        HelperOp::HostsSetBlock {
            entries: vec![entry("127.0.0.1", "loja.test"), entry("::1", "loja.test")],
        },
        HelperOp::TrustRemoveMachineRoot {
            thumbprint: "ab".repeat(20),
        },
        HelperOp::FirewallAdd {
            name: "Forgebox-nginx-lan".into(),
            port: 443,
            exe: root.root.join("bin/nginx/1.27.4/nginx.exe"),
        },
        HelperOp::FirewallRemove {
            name: "Forgebox-nginx-lan".into(),
        },
        HelperOp::DefenderExclude {
            path: root.root.clone(),
        },
        HelperOp::DefenderRemove {
            path: root.root.clone(),
        },
        HelperOp::FsSetCaseSensitive {
            path: root.root.join("www/loja"),
        },
    ];
    for op in ops {
        let name = op.name();
        let (code, response) = root.dry_run_op(op);
        assert!(
            response.ok,
            "{name}: {} ({:?})",
            response.message, response.code
        );
        assert_eq!(code, 0, "{name}");
        assert_eq!(response.details["dry_run"], true, "{name}");
    }
    // The plan describes the current state of this machine without changing it.
    let (_, hosts) = root.dry_run_op(HelperOp::HostsSetBlock {
        entries: vec![entry("127.0.0.1", "forgebox-never-added.test")],
    });
    assert!(
        hosts.details["path"]
            .as_str()
            .unwrap()
            .ends_with(r"drivers\etc\hosts")
    );
    assert_eq!(hosts.details["action"], "add");

    let lines = root.log_lines();
    assert_eq!(lines.len(), 10, "one helper.log line per request");
    assert!(
        lines
            .iter()
            .all(|l| l["dry_run"] == true && l["ok"] == true)
    );
    assert_eq!(lines[0]["op"], "nrpt.add");
}

/// The CA that `forge-tls` really generates (M1-E4-T4, ADR-0023) passes the helper's
/// machine-root policy (ADR-0024): CN `Forgebox Local CA ...`, critical Name Constraints
/// with local-use TLDs only (a configured `.dev` is left out by forge-tls), `localhost`
/// and loopback ranges.
#[test]
fn the_forge_tls_ca_is_planned_for_the_machine_store() {
    use forge_core::platform::CertThumbprint;
    use forge_tls::ca::{CaOptions, LocalCa};

    let root = Root::new();
    let ca = LocalCa::generate(&CaOptions::for_this_machine(
        vec![".test".into(), ".dev".into()],
        forge_core::Timestamp::now(),
    ))
    .unwrap();
    std::fs::write(root.root.join("certs/ca/forgebox-ca.crt"), ca.cert_pem()).unwrap();
    let thumbprint = CertThumbprint::of_der(ca.cert_der());
    let (code, response) = root.dry_run_op(HelperOp::TrustAddMachineRoot {
        thumbprint: thumbprint.to_hex().to_lowercase(),
    });
    assert_eq!(code, 0, "{} ({:?})", response.message, response.code);
    assert_eq!(response.details["thumbprint"], thumbprint.to_hex());
    assert_eq!(response.details["already_trusted"], false);
    let constraints = &response.details["name_constraints"];
    assert_eq!(constraints["critical"], true);
    assert_eq!(constraints["permitted_dns"], json!(["test", "localhost"]));
    assert_eq!(
        constraints["permitted_ip"],
        json!([{"addr": "127.0.0.0", "len": 8}, {"addr": "::1", "len": 128}])
    );
    assert!(
        response.details["subject"]
            .as_str()
            .unwrap()
            .starts_with("Forgebox Local CA "),
        "{}",
        response.details
    );

    // The same request for a CA nobody put in certs/ca/ is refused.
    let other = LocalCa::generate(&CaOptions::for_this_machine(
        vec![".test".into()],
        forge_core::Timestamp::now(),
    ))
    .unwrap();
    let (code, response) = root.dry_run_op(HelperOp::TrustAddMachineRoot {
        thumbprint: CertThumbprint::of_der(other.cert_der()).to_hex(),
    });
    assert_eq!(code, 1);
    assert_eq!(
        response.code.as_deref(),
        Some("not_found"),
        "{}",
        response.message
    );
}

#[test]
fn parameters_outside_the_policy_are_refused() {
    let root = Root::new();
    let r = &root.root;
    let cases: Vec<(HelperOp, &str)> = vec![
        // Namespace outside the configured TLDs (and not reserved for local use).
        (
            HelperOp::NrptAdd {
                namespace: ".com".into(),
                nameserver: "127.0.0.1".parse().unwrap(),
            },
            "forbidden",
        ),
        // NRPT pointing anywhere but the Forgebox resolver.
        (
            HelperOp::NrptAdd {
                namespace: ".test".into(),
                nameserver: "8.8.8.8".parse().unwrap(),
            },
            "forbidden",
        ),
        // Host with an invalid character.
        (
            HelperOp::HostsSetBlock {
                entries: vec![entry("127.0.0.1", "lo ja.test")],
            },
            "invalid_input",
        ),
        (
            HelperOp::HostsSetBlock {
                entries: vec![entry("127.0.0.1", "loja.test\r\n1.2.3.4 bank.com")],
            },
            "invalid_input",
        ),
        // IP other than loopback.
        (
            HelperOp::HostsSetBlock {
                entries: vec![entry("192.168.0.10", "loja.test")],
            },
            "forbidden",
        ),
        // Exe outside bin/.
        (
            HelperOp::FirewallAdd {
                name: "Forgebox-x".into(),
                port: 80,
                exe: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            },
            "forbidden",
        ),
        // Rule name without the prefix.
        (
            HelperOp::FirewallRemove {
                name: "Core Networking - DNS (UDP-Out)".into(),
            },
            "forbidden",
        ),
        // Invalid thumbprint.
        (
            HelperOp::TrustAddMachineRoot {
                thumbprint: "not-a-thumbprint".into(),
            },
            "invalid_input",
        ),
        // Defender path other than the root.
        (
            HelperOp::DefenderExclude {
                path: PathBuf::from(r"C:\"),
            },
            "forbidden",
        ),
        (
            HelperOp::DefenderExclude {
                path: r.join("www"),
            },
            "forbidden",
        ),
        // Installer outside tmp/downloads.
        (
            HelperOp::VcredistInstall {
                path: r.join("bin/nginx/1.27.4/nginx.exe"),
                sha256: "a".repeat(64),
            },
            "forbidden",
        ),
        // Case sensitivity on state/.
        (
            HelperOp::FsSetCaseSensitive {
                path: r.join("state"),
            },
            "forbidden",
        ),
    ];
    for (op, code) in cases {
        let name = op.name();
        let (exit, response) = root.dry_run_op(op);
        assert!(!response.ok, "{name} must be refused");
        assert_eq!(
            response.code.as_deref(),
            Some(code),
            "{name}: {}",
            response.message
        );
        assert_eq!(exit, 1, "{name}");
    }
}

#[test]
fn requests_outside_the_protocol_are_refused_with_a_code() {
    let root = Root::new();
    let id = RequestId::new();
    for (text, code) in [
        (
            json!({"v": 1, "request_id": id.to_string(), "root": root.root,
                   "op": {"name": "shell.run", "params": {"command": "cmd /c whoami"}}})
            .to_string(),
            "forbidden",
        ),
        (
            json!({"v": 1, "request_id": id.to_string(), "root": root.root,
                   "op": {"name": "nrpt.add", "params": {"namespace": ".test", "nameserver": "127.0.0.1", "gpo": "x"}}})
            .to_string(),
            "invalid_input",
        ),
        (
            json!({"v": 2, "request_id": id.to_string(), "root": root.root,
                   "op": {"name": "nrpt.remove", "params": {"namespace": ".test"}}})
            .to_string(),
            "invalid_input",
        ),
        ("{\"v\": 1, \"request_id\": ".to_owned(), "invalid_input"),
        ("".to_owned(), "invalid_input"),
        (
            // A valid request about another root.
            HelperRequest::new(
                std::env::temp_dir(),
                HelperOp::NrptRemove {
                    namespace: ".test".into(),
                },
            )
            .to_json()
            .unwrap(),
            "forbidden",
        ),
    ] {
        let (exit, response) = root.dry_run(&text);
        assert_eq!(exit, 1, "{text}");
        assert!(!response.ok);
        assert_eq!(response.code.as_deref(), Some(code), "{text}: {}", response.message);
    }
    let lines = root.log_lines();
    assert_eq!(lines.len(), 6);
    assert!(
        lines
            .iter()
            .all(|l| l["ok"] == false && l["code"].is_string())
    );
}

#[test]
fn command_line_misuse_is_a_usage_error() {
    let root = Root::new();
    for args in [
        &["--request", "-"][..],
        &[],
        &["--run", "calc.exe"],
        &["--request", "x.json", "--response"],
    ] {
        let out = root.run(args, "{}");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "no response for {args:?}");
    }
    let help = root.run(&["--help"], "");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--dry-run"));
}

#[test]
fn exchange_files_carry_a_real_run() {
    let root = Root::new();
    // What the daemon does before ShellExecuteExW: both files exist, the request is in.
    let request = HelperRequest::new(
        &root.root,
        HelperOp::FirewallRemove {
            name: "Forgebox-test-never-created".into(),
        },
    );
    let req = request_path(&root.root, request.request_id);
    let resp = response_path(&root.root, request.request_id);
    std::fs::write(&req, request.to_json().unwrap()).unwrap();
    std::fs::write(&resp, "").unwrap();
    let out = root.run(
        &[
            "--request",
            req.to_str().unwrap(),
            "--response",
            resp.to_str().unwrap(),
        ],
        "",
    );
    let response = HelperResponse::from_json(&std::fs::read_to_string(&resp).unwrap()).unwrap();
    assert_eq!(response.request_id, request.request_id);
    let elevated = forge_core::platform::windows::process_is_elevated().unwrap();
    if elevated {
        // Removing a rule that does not exist changes nothing.
        assert!(response.ok, "{}", response.message);
        assert_eq!(out.status.code(), Some(0));
    } else {
        assert_eq!(
            response.code.as_deref(),
            Some("needs_elevation"),
            "{}",
            response.message
        );
        assert_eq!(out.status.code(), Some(1));
    }
    assert!(out.stdout.is_empty(), "file mode writes nothing on stdout");

    // A response path outside the layout is never written.
    let stray = root.root.join("stray.json");
    std::fs::write(&stray, "").unwrap();
    let out = root.run(
        &[
            "--request",
            req.to_str().unwrap(),
            "--response",
            stray.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&stray).unwrap(), "");

    // A request file placed elsewhere is refused.
    let elsewhere = root.root.join(req.file_name().unwrap());
    std::fs::copy(&req, &elsewhere).unwrap();
    let out = root.run(&["--dry-run", "--request", elsewhere.to_str().unwrap()], "");
    assert_eq!(out.status.code(), Some(2));
}

/// Real VC++ Redistributable (Microsoft's permanent link), cached in
/// `C:\apps_src\Forgebox-wt\cache`: `vcredist.install --dry-run` checks its sha256 and its
/// Authenticode signature by Microsoft without installing anything. The file changes with
/// every release, so the hash is the one of the downloaded file (as the daemon does,
/// docs/11 §1); the signature is the integrity check.
#[test]
#[ignore = "downloads vc_redist.x64.exe (~25 MB) once into the shared cache"]
fn vcredist_dry_run_online() {
    let cache = Path::new(r"C:\apps_src\Forgebox-wt\cache");
    std::fs::create_dir_all(cache).unwrap();
    let cached = cache.join("vc_redist.x64.exe");
    if !cached.is_file() {
        let partial = cache.join("vc_redist.x64.exe.part");
        let status = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&partial)
            .arg(forge_core::platform::VCREDIST_X64_URL)
            .status()
            .expect("curl");
        assert!(status.success(), "download failed");
        std::fs::rename(&partial, &cached).unwrap();
    }
    let root = Root::new();
    let path = root.root.join("tmp/downloads/vc_redist.x64.exe");
    std::fs::copy(&cached, &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let sha: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    eprintln!("vc_redist.x64.exe: {} bytes, sha256 {sha}", bytes.len());
    let (code, response) = root.dry_run_op(HelperOp::VcredistInstall {
        path: path.clone(),
        sha256: sha.clone(),
    });
    assert!(response.ok, "{} ({:?})", response.message, response.code);
    assert_eq!(code, 0);
    assert_eq!(response.details["publisher"], "Microsoft Corporation");
    eprintln!("{}", response.message);

    // One wrong hex digit: refused before anything else.
    let mut wrong = sha.into_bytes();
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    let (code, response) = root.dry_run_op(HelperOp::VcredistInstall {
        path,
        sha256: String::from_utf8(wrong).unwrap(),
    });
    assert_eq!(code, 1);
    assert_eq!(response.code.as_deref(), Some("verification_failed"));
}
