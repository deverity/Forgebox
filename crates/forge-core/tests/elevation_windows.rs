//! The Windows elevation transport (ADR-0021) end to end, without a UAC prompt:
//! `WindowsElevation::without_elevation()` starts a fake helper (built from
//! `tests/fake_helper`) through the same `ShellExecuteExW` + exchange files path as the
//! real `runas` launch. The elevated launch itself needs a human (smoke M1).
#![cfg(windows)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use forge_core::helper::{HelperClient, HelperOp, HelperPolicy};
use forge_core::platform::windows::WindowsElevation;

/// Builds `tests/fake_helper` once per test binary into its own target dir.
fn fake_helper() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut hasher = DefaultHasher::new();
        manifest_dir.hash(&mut hasher);
        let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("fake-helper-{:016x}", hasher.finish()));
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .arg("build")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(manifest_dir.join("tests/fake_helper/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .env_remove("CARGO_MAKEFLAGS")
            .env_remove("MAKEFLAGS")
            .env_remove("MFLAGS")
            .output()
            .expect("running cargo to build the fake helper");
        assert!(
            output.status.success(),
            "building the fake helper failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let exe = target.join("debug").join("fake_helper.exe");
        assert!(exe.is_file(), "{} missing", exe.display());
        exe
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // A space in the root exercises the quoting of the command line.
        let root = dir.path().join("Forge box");
        std::fs::create_dir_all(&root).unwrap();
        Self { _dir: dir, root }
    }

    fn client(&self, elevation: WindowsElevation) -> HelperClient {
        let policy = HelperPolicy::new(&self.root, [".test"]).unwrap();
        HelperClient::new(Arc::new(elevation), fake_helper(), policy)
    }

    fn locks(&self) -> PathBuf {
        self.root.join("state").join("locks")
    }

    fn assert_no_exchange_files(&self) {
        let left: Vec<_> = std::fs::read_dir(self.locks())
            .map(|d| d.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "exchange files left behind: {left:?}");
    }
}

fn remove_rule(name: &str) -> HelperOp {
    HelperOp::FirewallRemove { name: name.into() }
}

#[test]
fn request_and_response_cross_through_the_files() {
    let fx = Fixture::new();
    let client = fx.client(WindowsElevation::without_elevation());
    let response = client.run(remove_rule("Forgebox-nginx")).unwrap();
    assert!(response.ok);
    assert_eq!(response.message, "fake helper ok");
    let bytes = response.details["request_bytes"].as_u64().unwrap();
    assert!(
        bytes > 50,
        "the helper read the whole request ({bytes} bytes)"
    );
    fx.assert_no_exchange_files();
}

#[test]
fn refusals_and_broken_helpers_are_errors() {
    let fx = Fixture::new();
    let client = fx.client(WindowsElevation::without_elevation());

    let err = client.run(remove_rule("Forgebox-fake-refuse")).unwrap_err();
    assert_eq!(err.code(), "forbidden", "{err}");
    assert!(err.to_string().contains("fake helper refused"), "{err}");

    let err = client.run(remove_rule("Forgebox-fake-silent")).unwrap_err();
    assert_eq!(err.code(), "internal", "{err}");
    assert!(err.to_string().contains("code 3"), "{err}");

    let err = client
        .run(remove_rule("Forgebox-fake-garbage"))
        .unwrap_err();
    assert_eq!(err.code(), "internal", "{err}");

    fx.assert_no_exchange_files();
}

#[test]
fn a_helper_that_does_not_finish_times_out() {
    let fx = Fixture::new();
    let client =
        fx.client(WindowsElevation::without_elevation().with_timeout(Duration::from_millis(500)));
    let started = Instant::now();
    let err = client.run(remove_rule("Forgebox-fake-sleep")).unwrap_err();
    assert_eq!(err.code(), "timeout", "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    fx.assert_no_exchange_files();
}

#[test]
fn parallel_runs_do_not_mix_answers() {
    let fx = Fixture::new();
    let client = fx.client(WindowsElevation::without_elevation());
    let threads: Vec<_> = (0..4)
        .map(|i| {
            let client = client.clone();
            std::thread::spawn(move || client.run(remove_rule(&format!("Forgebox-rule-{i}"))))
        })
        .collect();
    for thread in threads {
        assert!(thread.join().unwrap().unwrap().ok);
    }
    fx.assert_no_exchange_files();
}

#[test]
fn invalid_operations_never_start_the_helper() {
    let fx = Fixture::new();
    let client = fx.client(WindowsElevation::without_elevation());
    let err = client
        .run(HelperOp::DefenderExclude {
            path: PathBuf::from(r"C:\Windows"),
        })
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");
    assert!(!fx.locks().exists(), "no exchange was even prepared");
}
