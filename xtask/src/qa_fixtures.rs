//! `cargo xtask qa-fixtures`: fixtures recorded from a real `forgeboxd` (M2-E4-T1).
//!
//! The daemon boots in a temporary root seeded with fake binaries (the same `fake_php.rs`
//! the daemon and CLI tests use), only read-only Viewer methods are called (asserted
//! against `METHODS`), and the answers are written as normalized JSON: ULIDs and
//! timestamps become stable placeholders and the temporary root becomes `C:/Forgebox`.
//! `not_implemented.json` comes straight from `NOT_IMPLEMENTED` and needs no daemon, so
//! CI runs `--only not-implemented` and `git diff --exit-code` as a drift check. The other
//! files depend on the machine (versions, ports, clocks) and are refreshed by hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use forge_core::paths::ForgeRoot;
use forge_ipc::api::Role;
use forge_ipc::api::methods::METHODS;
use forge_ipc::client::CallOptions;
use forge_ipc::{Client, ClientOptions, Credentials};
use serde_json::{Value, json};

/// Default output folder, relative to the repository root.
pub const DEFAULT_OUT: &str = "apps/desktop/qa/fixtures/recorded";

/// Root shown in the recorded files.
pub const FIXTURE_ROOT: &str = "C:/Forgebox";
const FIXTURE_TIME: &str = "2026-01-01T00:00:00Z";
const FIXTURE_MS: u64 = 1_767_225_600_000;
const BACKSLASH: char = '\\';
const NOT_IMPLEMENTED_FILE: &str = "not_implemented.json";

/// Subsets of the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Only {
    /// Only `not_implemented.json` (deterministic, no daemon): what CI checks for drift.
    NotImplemented,
}

/// `(file, method, params)`: every call is a read-only Viewer method.
pub fn recorded_calls() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("daemon.status.json", "daemon.status", json!({})),
        ("services.status.json", "services.status", json!({})),
        ("services.ports.json", "services.ports", json!({})),
        ("sites.list.json", "sites.list", json!({})),
        (
            "sites.info.json",
            "sites.info",
            json!({"site": "demo.test"}),
        ),
        ("php.list.json", "php.list", json!({})),
        ("php.pool_status.json", "php.pool_status", json!({})),
        ("tools.list.json", "tools.list", json!({})),
        ("tools.search.json", "tools.search", json!({"text": ""})),
        ("tools.info.json", "tools.info", json!({"tool": "php"})),
        ("net.dns_status.json", "net.dns_status", json!({})),
        ("tls.ca_status.json", "tls.ca_status", json!({})),
        ("tls.list.json", "tls.list", json!({})),
        ("jobs.list.json", "jobs.list", json!({})),
        (
            "events.list.json",
            "events.list",
            json!({"after_seq": 0, "limit": 50}),
        ),
        (
            "logs.read.json",
            "logs.read",
            json!({"target": "daemon", "limit": 20}),
        ),
    ]
}

/// Fails when a recorded method is unknown, mutating or needs more than the Viewer role.
pub fn check_read_only() -> Result<()> {
    for (_, method, _) in recorded_calls() {
        let Some(info) = METHODS.iter().find(|m| m.name == method) else {
            bail!("recorded method {method} is not in METHODS");
        };
        if info.mutating {
            bail!("recorded method {method} is mutating");
        }
        if info.min_role != Role::Viewer {
            bail!("recorded method {method} is not a Viewer method");
        }
    }
    Ok(())
}

/// Rows of `NOT_IMPLEMENTED` as the JSON the fakeBridge reads.
pub fn not_implemented_json() -> String {
    let rows: Vec<Value> = forge_daemon::api::not_implemented::NOT_IMPLEMENTED
        .iter()
        .map(|(method, task, what)| json!({"method": method, "task": task, "what": what}))
        .collect();
    to_text(&Value::Array(rows))
}

fn to_text(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON value serializes");
    text.push('\n');
    text
}

/// Deterministic normalizer: same input, same output, whatever the run.
pub struct Normalizer {
    root_forms: Vec<String>,
    ulids: BTreeMap<String, String>,
}

impl Normalizer {
    /// `root`: the temporary installation root to hide (any slash style, any case).
    pub fn new(root: &Path) -> Self {
        let plain = forge_core::paths::plain_path(root)
            .to_string_lossy()
            .into_owned();
        // Plain, POSIX and the JSON-escaped forms found inside log lines (1 to 3 levels).
        let mut forms = vec![plain.replace(BACKSLASH, "/").to_lowercase()];
        for level in [1, 2, 4, 8] {
            forms.push(plain.replace(BACKSLASH, &"\\".repeat(level)).to_lowercase());
        }
        forms.sort();
        forms.dedup();
        // Longest first so `C:/a/b` wins over `C:/a`.
        forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        Self {
            root_forms: forms,
            ulids: BTreeMap::new(),
        }
    }

    pub fn normalize(&mut self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.string(s)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.normalize(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        let v = match (k.as_str(), v) {
                            ("pid", Value::Number(_)) => json!(4242),
                            (
                                "ts" | "at" | "started_at" | "created_at" | "updated_at",
                                Value::Number(_),
                            ) => {
                                json!(FIXTURE_MS)
                            }
                            _ => self.normalize(v),
                        };
                        (k.clone(), v)
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn placeholder(&mut self, ulid: &str) -> String {
        let next = self.ulids.len() + 1;
        self.ulids
            .entry(ulid.to_owned())
            .or_insert_with(|| format!("01FORGEBOXFIXTURE{next:09}"))
            .clone()
    }

    fn string(&mut self, s: &str) -> String {
        let text = self.replace_root(s);
        if is_ulid(&text) {
            return self.placeholder(&text);
        }
        // Timestamps and ULIDs embedded in longer strings (log lines, `job <id> failed`).
        let text = timestamp_re().replace_all(&text, FIXTURE_TIME);
        self.embedded_ulids(&text)
    }

    fn replace_root(&self, s: &str) -> String {
        let lower = s.to_lowercase();
        // Lowercasing can change byte lengths for exotic characters: then leave the text.
        if lower.len() != s.len() {
            return s.to_owned();
        }
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        'outer: while i < s.len() {
            for form in &self.root_forms {
                if lower[i..].starts_with(form.as_str()) {
                    out.push_str(FIXTURE_ROOT);
                    i += form.len();
                    continue 'outer;
                }
            }
            let ch = s[i..].chars().next().expect("index on a char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
        if out.contains(FIXTURE_ROOT) {
            out = slash_paths(&out);
        }
        out
    }

    fn embedded_ulids(&mut self, s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            if i + 26 <= s.len()
                && s.is_char_boundary(i + 26)
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
                && is_ulid(&s[i..i + 26])
                && (i + 26 == s.len() || !bytes[i + 26].is_ascii_alphanumeric())
            {
                let id = self.placeholder(&s[i..i + 26]);
                out.push_str(&id);
                i += 26;
            } else {
                let ch = s[i..].chars().next().expect("index on a char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        out
    }
}

/// Backslash runs (path separators, possibly JSON-escaped several levels) become `/`,
/// except the ones that escape a quote.
fn slash_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != BACKSLASH {
            out.push(c);
            continue;
        }
        let mut run = 1;
        while chars.next_if_eq(&BACKSLASH).is_some() {
            run += 1;
        }
        if chars.peek() == Some(&'"') {
            out.extend(std::iter::repeat_n(BACKSLASH, run));
        } else {
            out.push('/');
        }
    }
    out
}

fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
        && s.as_bytes()[0] <= b'7'
}

fn timestamp_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})")
            .expect("static regex")
    })
}

/// Kills the daemon (and its children) if the recording leaves early.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.0.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// `forgeboxd.exe` of the same target folder as this `xtask`, rebuilt first.
fn daemon_exe(repo: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "forge-daemon", "--bin", "forgeboxd"])
        .current_dir(repo)
        .status()
        .context("running cargo build -p forge-daemon")?;
    if !status.success() {
        bail!("cargo build -p forge-daemon failed");
    }
    // `target/debug/xtask.exe` or `target/debug/deps/xtask-<hash>.exe` (tests).
    let mut dir = std::env::current_exe()?;
    dir.pop();
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.pop();
    }
    let exe = dir.join("forgeboxd.exe");
    if !exe.is_file() {
        bail!("{} does not exist", exe.display());
    }
    Ok(exe)
}

/// Compiles the daemon tests' fake PHP (`php.exe` and a FastCGI `php-cgi.exe`).
fn compile_fake_php(repo: &Path, work: &Path) -> Result<PathBuf> {
    let source = repo.join("crates/forge-daemon/tests/fixtures/fake_php.rs");
    let exe = work.join("fake_php.exe");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--edition", "2024", "-O", "-o"])
        .arg(&exe)
        .arg(&source)
        .output()
        .context("running rustc")?;
    if !output.status.success() {
        bail!(
            "compiling fake_php: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(exe)
}

/// Seeds the root: forgebox.toml for tests, a fake PHP, two parked projects.
fn seed(root: &ForgeRoot, fake_php: &Path) -> Result<()> {
    std::fs::create_dir_all(root.etc())?;
    let databases = (0..16)
        .map(|_| free_port().map(|p| p.to_string()))
        .collect::<Result<Vec<_>>>()?
        .join(", ");
    let toml = format!(
        "[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [{}]\ninternal = [{}]\ndatabases = [{databases}]\n",
        free_port()?,
        free_port()?,
    );
    std::fs::write(root.forgebox_toml(), toml)?;
    let php = root.bin().join("php").join("8.4.27-nts-vs17-x64");
    std::fs::create_dir_all(&php)?;
    std::fs::copy(fake_php, php.join("php.exe"))?;
    std::fs::copy(fake_php, php.join("php-cgi.exe"))?;
    let www = root.path().join("www");
    for (name, index) in [("demo", "index.php"), ("shop", "public/index.php")] {
        let file = www.join(name).join(index);
        std::fs::create_dir_all(file.parent().expect("file has a parent"))?;
        std::fs::write(file, "<?php echo 'ok';\n")?;
    }
    Ok(())
}

async fn connect(root: &ForgeRoot, daemon: &mut DaemonGuard) -> Result<Client> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(Some(status)) = daemon.0.try_wait() {
            bail!("forgeboxd exited during boot ({status})");
        }
        if root.ipc_token().is_file()
            && let Ok(credentials) =
                Credentials::local(root, "forge-xtask", env!("CARGO_PKG_VERSION"))
        {
            let transport = forge_ipc::transport::local_transport(root);
            let options = ClientOptions {
                call_timeout: Duration::from_secs(30),
                connect_timeout: Duration::from_secs(5),
                ..ClientOptions::default()
            };
            if let Ok(client) = Client::connect(transport, credentials, options).await {
                return Ok(client);
            }
        }
        if Instant::now() >= deadline {
            bail!("forgeboxd did not answer within 60 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Boots a daemon on a temporary root, records every call into `out`, stops the daemon.
pub fn record_daemon(repo: &Path, out: &Path) -> Result<()> {
    check_read_only()?;
    let exe = daemon_exe(repo)?;
    let temp = tempfile::Builder::new().prefix("fbqa-").tempdir()?;
    // Long form: the daemon reports the root without 8.3 short names.
    let temp_path = forge_core::paths::plain_path(&temp.path().canonicalize()?);
    let work = tempfile::Builder::new().prefix("fbqa-bin-").tempdir()?;
    let fake_php = compile_fake_php(repo, work.path())?;
    let root = ForgeRoot::new(&temp_path)?;
    seed(&root, &fake_php)?;
    let registry = forge_core::paths::plain_path(&repo.join("registry"));

    let child = Command::new(&exe)
        .arg("--root")
        .arg(&temp_path)
        .args(["--log-level", "info", "--shutdown-timeout", "20"])
        .env("FORGEBOX_OFFICIAL_REGISTRY", registry)
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting forgeboxd")?;
    let mut guard = DaemonGuard(child);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let written = runtime.block_on(async {
        let client = connect(&root, &mut guard).await?;
        // Let the boot scan register the parked projects.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut normalizer = Normalizer::new(&temp_path);
        let mut written = Vec::new();
        for (file, method, params) in recorded_calls() {
            let answer = client
                .call_raw(method, params, CallOptions::default())
                .await
                .with_context(|| format!("calling {method}"))?;
            written.push((file, to_text(&normalizer.normalize(&answer))));
        }
        // Teardown (not recorded): ask the daemon to stop.
        let _ = client
            .call_raw("daemon.stop", json!({}), CallOptions::default())
            .await;
        client.close();
        Ok::<_, anyhow::Error>(written)
    })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while guard.0.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(guard); // kills the process tree if it is still there
    for (file, text) in written {
        crate::write_file(&out.join(file), &text)?;
        println!("wrote {}", out.join(file).display());
    }
    Ok(())
}

/// Entry point of `cargo xtask qa-fixtures`.
pub fn run(repo: &Path, out: Option<&Path>, only: Option<Only>) -> Result<()> {
    let out = out.map_or_else(|| repo.join(DEFAULT_OUT), Path::to_path_buf);
    check_read_only()?;
    crate::write_file(&out.join(NOT_IMPLEMENTED_FILE), &not_implemented_json())?;
    println!("wrote {}", out.join(NOT_IMPLEMENTED_FILE).display());
    if only.is_none() {
        record_daemon(repo, &out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizer_is_deterministic_and_hides_the_root() {
        let root = Path::new(r"C:\Users\x\AppData\Local\Temp\fbqa-abc");
        let input = json!({
            "id": "01JZ3K4M5N6P7Q8R9S0T1V2W3X",
            "other": "01JZ3K4M5N6P7Q8R9S0T1V2W3Y",
            "again": "01JZ3K4M5N6P7Q8R9S0T1V2W3X",
            "at": "2026-11-04T12:34:56.789+00:00",
            "pid": 31337,
            "path": r"C:\Users\x\AppData\Local\Temp\fbqa-abc\bin\php",
            "posix": "c:/users/x/appdata/local/temp/FBQA-ABC/www/demo",
            "msg": "job 01JZ3K4M5N6P7Q8R9S0T1V2W3Y failed",
            "list": [1, true, null],
        });
        let a = Normalizer::new(root).normalize(&input);
        let b = Normalizer::new(root).normalize(&input);
        assert_eq!(a, b);
        // Objects iterate by key: "again" is met first, then "id", then "msg"/"other".
        assert_eq!(a["again"], a["id"]);
        assert_ne!(a["other"], a["id"]);
        assert_eq!(
            a["msg"],
            format!("job {} failed", a["other"].as_str().unwrap())
        );
        assert_eq!(a["at"], FIXTURE_TIME);
        assert_eq!(a["pid"], 4242);
        assert_eq!(a["path"], "C:/Forgebox/bin/php");
        assert_eq!(a["posix"], "C:/Forgebox/www/demo");
        assert_eq!(a["list"], json!([1, true, null]));
        assert!(a["id"].as_str().unwrap().starts_with("01FORGEBOXFIXTURE"));
        // Idempotent: normalizing the output changes nothing.
        assert_eq!(Normalizer::new(root).normalize(&a), a);
    }

    #[test]
    fn recorded_methods_are_read_only_viewer_methods() {
        check_read_only().unwrap();
        for (file, method, _) in recorded_calls() {
            assert_eq!(file, format!("{method}.json"));
            let info = METHODS.iter().find(|m| m.name == method).unwrap();
            assert!(!info.mutating, "{method}");
        }
    }

    #[test]
    fn not_implemented_json_mirrors_the_table() {
        let rows: Vec<Value> = serde_json::from_str(&not_implemented_json()).unwrap();
        assert_eq!(
            rows.len(),
            forge_daemon::api::not_implemented::NOT_IMPLEMENTED.len()
        );
        assert_eq!(not_implemented_json(), not_implemented_json());
    }

    #[test]
    fn only_not_implemented_writes_just_that_file() {
        let dir = tempfile::tempdir().unwrap();
        run(
            &crate::repo_root(),
            Some(dir.path()),
            Some(Only::NotImplemented),
        )
        .unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![NOT_IMPLEMENTED_FILE.to_owned()]);
    }

    /// Boots a real `forgeboxd` on a temporary root: every file is written and no daemon
    /// is left running.
    #[cfg(windows)]
    #[test]
    fn recording_writes_every_file_and_leaves_no_daemon() {
        let repo = crate::repo_root();
        let dir = tempfile::tempdir().unwrap();
        run(&repo, Some(dir.path()), None).unwrap();
        for (file, _, _) in recorded_calls() {
            let text = std::fs::read_to_string(dir.path().join(file)).unwrap();
            serde_json::from_str::<Value>(&text).unwrap();
            assert!(!text.contains("fbqa-"), "{file} leaks the temp root");
        }
        assert!(dir.path().join(NOT_IMPLEMENTED_FILE).is_file());
        let leftover = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'forgeboxd.exe' } | ForEach-Object { $_.CommandLine }",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&leftover.stdout);
        assert!(!text.contains("fbqa-"), "daemon left running: {text}");
    }
}
