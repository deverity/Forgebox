//! Official binaries for the `*_online` tests: downloaded once into the shared cache
//! (`C:\apps_src\Forgebox-wt\cache`, or `FORGEBOX_TEST_CACHE`), checked against the
//! sha256 of the registry, extracted once under `FORGEBOX_TEST_TMP`
//! (`C:\apps_src\Forgebox-wt\tmp\daemon-extract`) and copied into a test root.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// A release archive of the registry.
pub struct Archive {
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Folder inside the zip that holds the files (`""` when they are at the top).
    pub inner: &'static str,
}

/// PHP 8.4.27 NTS VS17 x64 (`releases.json` of downloads.php.net).
pub const PHP_8_4: Archive = Archive {
    file: "php-8.4.27-1-nts-Win32-vs17-x64.zip",
    url: "https://downloads.php.net/~windows/releases/archives/php-8.4.27-nts-Win32-vs17-x64.zip",
    sha256: "dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7",
    inner: "",
};

/// PHP 8.3.36 NTS VS16 x64 (`releases.json` of downloads.php.net).
pub const PHP_8_3: Archive = Archive {
    file: "php-8.3.36-1-nts-Win32-vs16-x64.zip",
    url: "https://downloads.php.net/~windows/releases/archives/php-8.3.36-nts-Win32-vs16-x64.zip",
    sha256: "1a097d52b7a68622cdc723073b61cc70caf79c941218ff65b2d8441dbc6b10d4",
    inner: "",
};

/// nginx 1.30.6 (curated sha256 of `registry/tools/nginx.yaml`).
pub const NGINX_1_30: Archive = Archive {
    file: "nginx-1.30.6.zip",
    url: "https://nginx.org/download/nginx-1.30.6.zip",
    sha256: "c6ee35edb0a861063323f709192ae50d1eefaf2ea7d00678bd2b22d8542dc61e",
    inner: "nginx-1.30.6",
};

/// MySQL 8.4.13 (curated sha256 of `registry/tools/mysql.yaml`).
pub const MYSQL_8_4: Archive = Archive {
    file: "mysql-8.4.13-winx64.zip",
    url: "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.13-winx64.zip",
    sha256: "e9cd98b1d23f122987b1afca26ad942efec5891481103d48ae3cde237fa3ea54",
    inner: "mysql-8.4.13-winx64",
};

/// PostgreSQL 17.11 EDB binaries (curated sha256 of `registry/tools/postgresql.yaml`).
pub const POSTGRESQL_17: Archive = Archive {
    file: "postgresql-17.11-1-windows-x64-binaries.zip",
    url: "https://get.enterprisedb.com/postgresql/postgresql-17.11-1-windows-x64-binaries.zip",
    sha256: "6eabdf00d2893713b75db4336a23c3fdf505f056e217ec6e2e95d901750cfea3",
    inner: "pgsql",
};

/// Mailpit 1.32.0 (curated sha256 of `registry/tools/mailpit.yaml`).
pub const MAILPIT_1_32: Archive = Archive {
    file: "mailpit-windows-amd64.zip",
    url: "https://github.com/axllent/mailpit/releases/download/v1.32.0/mailpit-windows-amd64.zip",
    sha256: "16915767a6c95dca569336e8f561f91cf355e9a33c57b91deedcc10a4bba7fd4",
    inner: "",
};

fn shared_dir(var: &str, default: &str) -> PathBuf {
    let dir = std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn sha256_hex(path: &Path) -> String {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The extracted files of `archive` (downloaded, checked and extracted once).
pub fn extracted(archive: &Archive) -> PathBuf {
    let cache = shared_dir("FORGEBOX_TEST_CACHE", r"C:\apps_src\Forgebox-wt\cache");
    let tmp = shared_dir(
        "FORGEBOX_TEST_TMP",
        r"C:\apps_src\Forgebox-wt\tmp\daemon-extract",
    );
    let zip = cache.join(archive.file);
    if !zip.is_file() {
        let status = Command::new("curl.exe")
            .args(["-fsSL", "-o"])
            .arg(&zip)
            .arg(archive.url)
            .status()
            .expect("curl.exe");
        assert!(status.success(), "download of {} failed", archive.url);
    }
    assert_eq!(
        sha256_hex(&zip),
        archive.sha256,
        "{} does not match",
        zip.display()
    );
    let dir = tmp.join(archive.file.trim_end_matches(".zip"));
    let marker = dir.join(".extracted");
    if !marker.is_file() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&zip)
            .arg("-C")
            .arg(&dir)
            .status()
            .expect("tar");
        assert!(status.success());
        std::fs::write(&marker, archive.sha256).unwrap();
    }
    if archive.inner.is_empty() {
        dir
    } else {
        dir.join(archive.inner)
    }
}

/// Copies a folder recursively.
pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        if entry.file_name() == ".extracted" {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
