//! Real PHP builds for the `*_online` tests: the current NTS x64 build of a series from
//! `https://windows.php.net/downloads/releases/releases.json`, verified against the
//! SHA-256 of that listing, cached and extracted once.
//!
//! * downloads: `FORGEBOX_TEST_CACHE`, else `C:\apps_src\Forgebox-wt\cache` when it
//!   exists, else `<CARGO_TARGET_TMPDIR>/download-cache` (a cached zip is reused when its
//!   SHA-256 still matches);
//! * extraction: `FORGEBOX_TEST_TMP`, else `C:\apps_src\Forgebox-wt\tmp` when it exists,
//!   else the system temp dir, in `php-extract/<zip name>/` (reused when its marker holds
//!   the same SHA-256).
//!
//! PHP 8.4+ is built with VS17 (`nts-vs17-x64`); 8.3 and older with VS16
//! (`nts-vs16-x64`): the newest compiler listed is taken.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

const RELEASES: &str = "https://windows.php.net/downloads/releases/";

/// An extracted PHP build.
#[derive(Debug, Clone)]
pub struct PhpBuild {
    /// Folder with `php-cgi.exe`.
    pub dir: PathBuf,
    /// `releases.json` version (`8.3.36-1`: `-1` is a rebuild of the Windows package).
    pub release: String,
    /// What `PHP_VERSION` reports (`8.3.36`).
    pub php_version: String,
    /// `8.3`.
    pub series: String,
    /// `nts-vs16-x64`.
    pub build: String,
}

impl PhpBuild {
    pub fn php_cgi(&self) -> PathBuf {
        self.dir.join("php-cgi.exe")
    }
}

fn system32() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32")
}

fn dir_from(env: &str, shared: &str, fallback: PathBuf) -> PathBuf {
    if let Some(dir) = std::env::var_os(env) {
        return PathBuf::from(dir);
    }
    let shared = Path::new(shared);
    if shared.is_dir() {
        return shared.to_path_buf();
    }
    fallback
}

fn cache_dir() -> PathBuf {
    dir_from(
        "FORGEBOX_TEST_CACHE",
        r"C:\apps_src\Forgebox-wt\cache",
        Path::new(env!("CARGO_TARGET_TMPDIR")).join("download-cache"),
    )
}

fn extract_root() -> PathBuf {
    dir_from(
        "FORGEBOX_TEST_TMP",
        r"C:\apps_src\Forgebox-wt\tmp",
        std::env::temp_dir(),
    )
    .join("php-extract")
}

fn curl(url: &str, out: &Path) -> bool {
    Command::new(system32().join("curl.exe"))
        // Best-effort revocation: corporate networks often block CRL/OCSP endpoints; the
        // chain is still validated and the zip is checked by SHA-256.
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "2",
            "--ssl-revoke-best-effort",
        ])
        .arg("-o")
        .arg(out)
        .arg(url)
        .status()
        .is_ok_and(|s| s.success())
}

fn sha256_hex(path: &Path) -> String {
    use std::io::Read;

    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path).unwrap();
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let read = file.read(&mut buf).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `releases.json`, fresh when the network allows, else the cached copy.
fn releases(cache: &Path) -> serde_json::Value {
    let listing = cache.join("php-releases.json");
    let partial = cache.join(format!("php-releases.json.{}.part", std::process::id()));
    if curl(&format!("{RELEASES}releases.json"), &partial) {
        std::fs::rename(&partial, &listing).unwrap();
    } else {
        let _ = std::fs::remove_file(&partial);
        assert!(
            listing.is_file(),
            "cannot download releases.json and no cached copy"
        );
        println!("using the cached {}", listing.display());
    }
    serde_json::from_slice(&std::fs::read(&listing).unwrap()).unwrap()
}

/// Downloads (or reuses) and extracts (or reuses) the current NTS x64 build of `series`.
pub fn php_build(series: &str) -> PhpBuild {
    static BUILDS: OnceLock<Mutex<std::collections::HashMap<String, PhpBuild>>> = OnceLock::new();
    let builds = BUILDS.get_or_init(Default::default);
    let mut builds = builds.lock().unwrap();
    if let Some(build) = builds.get(series) {
        return build.clone();
    }
    let cache = cache_dir();
    std::fs::create_dir_all(&cache).unwrap();
    let json = releases(&cache);
    let entry = &json[series];
    let release = entry["version"]
        .as_str()
        .unwrap_or_else(|| panic!("series {series} not in releases.json"))
        .to_owned();
    let build = ["nts-vs17-x64", "nts-vs16-x64"]
        .into_iter()
        .find(|key| entry[*key]["zip"]["path"].is_string())
        .unwrap_or_else(|| panic!("no NTS x64 build for {series}"))
        .to_owned();
    let zip = &entry[build.as_str()]["zip"];
    let name = zip["path"].as_str().unwrap().to_owned();
    let expected = zip["sha256"].as_str().expect("sha256").to_ascii_lowercase();
    assert!(!name.contains(['/', '\\']), "unexpected path {name}");

    let file = cache.join(&name);
    if file.is_file() && sha256_hex(&file) == expected {
        println!("using cached {} (sha256 {expected})", file.display());
    } else {
        let partial = cache.join(format!("{name}.part"));
        assert!(
            curl(&format!("{RELEASES}{name}"), &partial),
            "download of {name} failed"
        );
        let actual = sha256_hex(&partial);
        assert_eq!(actual, expected, "sha256 mismatch for {name}");
        std::fs::rename(&partial, &file).unwrap();
        println!("downloaded {} (sha256 {expected} verified)", file.display());
    }

    let root = extract_root();
    std::fs::create_dir_all(&root).unwrap();
    let dir = root.join(name.trim_end_matches(".zip"));
    let marker = dir.join(".forgebox-extracted");
    let reuse = std::fs::read_to_string(&marker).is_ok_and(|m| m.trim() == expected)
        && dir.join("php-cgi.exe").is_file();
    if reuse {
        println!("using extracted {}", dir.display());
    } else {
        let staging = root.join(format!(
            "{}.partial-{}",
            name.trim_end_matches(".zip"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).unwrap();
        let status = Command::new(system32().join("tar.exe"))
            .arg("-xf")
            .arg(&file)
            .arg("-C")
            .arg(&staging)
            .status()
            .unwrap();
        assert!(status.success(), "extracting {}", file.display());
        assert!(staging.join("php-cgi.exe").is_file(), "php-cgi.exe missing");
        std::fs::write(staging.join(".forgebox-extracted"), &expected).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        if std::fs::rename(&staging, &dir).is_err() {
            // Another test process won the race: use its copy.
            let _ = std::fs::remove_dir_all(&staging);
            assert!(dir.join("php-cgi.exe").is_file());
        }
        println!("extracted {} to {}", name, dir.display());
    }
    let php_version = release
        .split_once('-')
        .map_or(release.as_str(), |(v, _)| v)
        .to_owned();
    let found = PhpBuild {
        dir,
        release,
        php_version,
        series: series.to_owned(),
        build,
    };
    builds.insert(series.to_owned(), found.clone());
    found
}
