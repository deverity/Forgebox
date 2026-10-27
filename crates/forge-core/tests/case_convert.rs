//! Conversion of existing trees to case-sensitive and back, against real NTFS
//! (docs/15 §3.1): every directory ends up flagged, content and attributes survive,
//! failures (file in use, folder in use, cancellation) restore the original tree, and an
//! interrupted conversion is recovered.
//!
//! Skipped with a message when `%TEMP%` cannot use the flag, unless
//! `FORGEBOX_REQUIRE_CASE_SENSITIVE=1` (NTFS dev machines and CI).
//!
//! `laragon_www_copy_roundtrip` (ignored) is the acceptance test of M1-E4-T6: it copies
//! projects of `C:\laragon\www` (read only) into a temporary folder and converts the copy.
#![cfg(windows)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use forge_core::fs::case::{
    ConvertOptions, OLD_SUFFIX, RecoverOutcome, TMP_SUFFIX, convert_tree, recover, tree_flags,
};
use forge_core::platform::{self, CaseSensitiveFs, CaseSensitivitySupport};
use sha2::{Digest, Sha256};

/// `FILE_SHARE_READ`: opening with only this share mode blocks renames of the file.
const SHARE_READ_ONLY: u32 = 0x1;
/// `FILE_FLAG_BACKUP_SEMANTICS`: needed to open a directory handle.
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_READONLY: u32 = 0x1;

fn cs() -> std::sync::Arc<dyn CaseSensitiveFs> {
    struct Shared(platform::Platform);
    impl CaseSensitiveFs for Shared {
        fn support(&self, dir: &Path) -> CaseSensitivitySupport {
            self.0.case_sensitive_fs().support(dir)
        }
        fn get(&self, dir: &Path) -> forge_core::Result<bool> {
            self.0.case_sensitive_fs().get(dir)
        }
        fn set(&self, dir: &Path, enabled: bool) -> forge_core::Result<()> {
            self.0.case_sensitive_fs().set(dir, enabled)
        }
        fn info(&self, dir: &Path) -> platform::CaseFsInfo {
            self.0.case_sensitive_fs().info(dir)
        }
        fn capture_dir_meta(&self, dir: &Path) -> Option<platform::DirMeta> {
            self.0.case_sensitive_fs().capture_dir_meta(dir)
        }
        fn apply_dir_meta(&self, dir: &Path, meta: &platform::DirMeta) -> forge_core::Result<()> {
            self.0.case_sensitive_fs().apply_dir_meta(dir, meta)
        }
    }
    std::sync::Arc::new(Shared(platform::current()))
}

/// A temp dir whose volume supports the flag, or `None` (skip).
fn supported_tempdir() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    match cs().support(dir.path()) {
        CaseSensitivitySupport::Supported => Some(dir),
        CaseSensitivitySupport::Unsupported(reason) => {
            if std::env::var("FORGEBOX_REQUIRE_CASE_SENSITIVE").as_deref() == Ok("1") {
                panic!("case sensitivity required but unsupported: {reason}");
            }
            eprintln!("SKIPPED: case sensitivity unsupported here: {reason}");
            None
        }
    }
}

fn sha256(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).unwrap();
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

/// Every entry under `root` (relative, `/`): `d` for directories, `l:<target>` for
/// junctions/links, `f:<size>:<sha256>` for files. Links are not followed.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for entry in fs::read_dir(root.join(&rel)).unwrap() {
            let entry = entry.unwrap();
            let rel_child = rel.join(entry.file_name());
            let key = rel_child.to_string_lossy().replace('\\', "/");
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if platform::is_reparse_point(&meta) {
                let target = platform::current()
                    .link_fs()
                    .read_junction(&entry.path())
                    .map(|t| t.display().to_string())
                    .unwrap_or_else(|_| "?".into());
                out.insert(key, format!("l:{target}"));
            } else if meta.is_dir() {
                out.insert(key, "d".into());
                stack.push(rel_child);
            } else {
                out.insert(key, format!("f:{}:{}", meta.len(), sha256(&entry.path())));
            }
        }
    }
    out
}

/// `(directories, sensitive)` over the whole tree (root included).
fn flags(root: &Path) -> (u64, u64) {
    let f = tree_flags(cs().as_ref(), root, usize::MAX, u64::MAX).unwrap();
    assert!(!f.truncated);
    (f.directories, f.sensitive)
}

fn no_leftovers(dir: &Path) {
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    for suffix in [TMP_SUFFIX, OLD_SUFFIX] {
        let p = dir.with_file_name(format!("{name}{suffix}"));
        assert!(!p.exists(), "leftover {}", p.display());
    }
}

/// A small project tree with the interesting cases: nested folders, an empty folder, a
/// hidden folder (`.git`), a read-only file, a binary file and a junction to a folder
/// outside the tree.
fn build_tree(base: &Path) -> PathBuf {
    let www = base.join("www");
    let app = www.join("Loja");
    fs::create_dir_all(app.join("app/Http/Controllers")).unwrap();
    fs::create_dir_all(app.join("views")).unwrap();
    fs::create_dir_all(app.join("storage/empty")).unwrap();
    fs::create_dir_all(app.join(".git/objects")).unwrap();
    fs::write(app.join("index.php"), "<?php require 'views/Header.php';\n").unwrap();
    fs::write(app.join("views/Header.php"), "<header></header>\n").unwrap();
    fs::write(
        app.join("app/Http/Controllers/HomeController.php"),
        "<?php\nnamespace App\\Http\\Controllers;\nclass HomeController {}\n",
    )
    .unwrap();
    fs::write(app.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let blob: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(app.join("logo.bin"), &blob).unwrap();
    let ro = app.join("LICENSE");
    fs::write(&ro, "MIT\n").unwrap();
    let mut perms = fs::metadata(&ro).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&ro, perms).unwrap();
    // Hide .git the way Git for Windows does.
    let status = std::process::Command::new("attrib")
        .arg("+h")
        .arg(app.join(".git"))
        .status()
        .unwrap();
    assert!(status.success());
    // A junction to a folder outside the tree.
    let outside = base.join("shared");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("Shared.txt"), "shared\n").unwrap();
    platform::current()
        .link_fs()
        .create_junction(&app.join("shared"), &outside)
        .unwrap();
    fs::write(www.join("index.html"), "<h1>www</h1>\n").unwrap();
    www
}

fn convert(dir: &Path, enable: bool) -> forge_core::Result<forge_core::fs::case::ConvertReport> {
    convert_tree(
        cs().as_ref(),
        dir,
        enable,
        &ConvertOptions::default(),
        &mut |_| {},
        &|| false,
    )
}

#[test]
fn convert_and_revert_keep_everything_and_flip_case_sensitivity() {
    let Some(base) = supported_tempdir() else {
        return;
    };
    let www = build_tree(base.path());
    let before = snapshot(&www);
    let git_dir = www.join("Loja/.git");
    let views_mtime = fs::metadata(www.join("Loja/views"))
        .unwrap()
        .modified()
        .unwrap();
    let (dirs, sensitive) = flags(&www);
    assert_eq!(sensitive, 0, "temp folders start insensitive");

    let report = convert(&www, true).unwrap();
    assert!(report.enabled && !report.already && !report.recovered);
    assert_eq!(report.stats.directories, dirs);
    assert_eq!(report.stats.files, 7);
    assert_eq!(report.stats.links, 1);
    assert!(report.sampled >= 7);
    assert_eq!(report.leftover, None);
    no_leftovers(&www);

    // Same content, every directory flagged.
    assert_eq!(snapshot(&www), before);
    assert_eq!(flags(&www), (dirs, dirs));
    // Directory attributes and timestamps were copied to the recreated folders.
    let git_attrs = fs::metadata(&git_dir).unwrap().file_attributes();
    assert_ne!(git_attrs & ATTR_HIDDEN, 0, ".git stays hidden");
    assert_eq!(
        fs::metadata(www.join("Loja/views"))
            .unwrap()
            .modified()
            .unwrap(),
        views_mtime
    );
    let ro = fs::metadata(www.join("Loja/LICENSE")).unwrap();
    assert_ne!(ro.file_attributes() & ATTR_READONLY, 0);
    // The junction still points outside and its target was not converted.
    assert!(
        platform::current()
            .link_fs()
            .is_junction(&www.join("Loja/shared"))
            .unwrap()
    );
    assert!(!cs().get(&base.path().join("shared")).unwrap());

    // Case is distinguished at every level now.
    for dir in [www.clone(), www.join("Loja/app/Http/Controllers")] {
        fs::write(dir.join("A.txt"), "upper").unwrap();
        fs::write(dir.join("a.txt"), "lower").unwrap();
        assert_eq!(fs::read_to_string(dir.join("A.txt")).unwrap(), "upper");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "lower");
    }
    assert!(!www.join("Loja/views/header.php").exists());
    assert!(www.join("Loja/views/Header.php").exists());

    // Converting again is a no-op.
    let again = convert(&www, true).unwrap();
    assert!(again.already);

    // Reverting is refused while names differ only by case; nothing moves.
    let err = convert(&www, false).unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");
    assert!(err.to_string().contains("A.txt + a.txt"), "{err}");
    no_leftovers(&www);
    assert_eq!(flags(&www), (dirs, dirs));
    for dir in [www.clone(), www.join("Loja/app/Http/Controllers")] {
        fs::remove_file(dir.join("A.txt")).unwrap();
        fs::remove_file(dir.join("a.txt")).unwrap();
    }

    let reverted = convert(&www, false).unwrap();
    assert!(!reverted.enabled);
    assert_eq!(snapshot(&www), before);
    assert_eq!(flags(&www), (dirs, 0));
    no_leftovers(&www);
    fs::write(www.join("Loja/X.txt"), "x").unwrap();
    assert!(www.join("Loja/x.txt").exists(), "insensitive again");
    fs::remove_file(www.join("Loja/X.txt")).unwrap();

    // read-only files must be writable again for the temp dir cleanup
    let mut perms = fs::metadata(www.join("Loja/LICENSE"))
        .unwrap()
        .permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(www.join("Loja/LICENSE"), perms).unwrap();
}

#[test]
fn file_in_use_aborts_and_restores_the_tree() {
    let Some(base) = supported_tempdir() else {
        return;
    };
    let www = build_tree(base.path());
    let before = snapshot(&www);
    let (dirs, _) = flags(&www);
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ_ONLY)
        .open(www.join("Loja/views/Header.php"))
        .unwrap();

    let err = convert(&www, true).unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");
    assert!(err.to_string().contains("Header.php"), "{err}");
    drop(locked);

    assert_eq!(snapshot(&www), before);
    assert_eq!(flags(&www), (dirs, 0));
    no_leftovers(&www);
    // and it works once the file is closed
    convert(&www, true).unwrap();
    assert_eq!(flags(&www), (dirs, dirs));
}

#[test]
fn folder_in_use_fails_the_swap_and_restores_the_tree() {
    let Some(base) = supported_tempdir() else {
        return;
    };
    let www = build_tree(base.path());
    let before = snapshot(&www);
    // A handle on a subfolder without FILE_SHARE_DELETE (like a shell whose current
    // directory is inside) does not stop files from moving out, but the folder tree can
    // no longer be renamed: the swap fails and everything goes back.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ_ONLY)
        .custom_flags(BACKUP_SEMANTICS)
        .open(www.join("Loja/app"))
        .unwrap();
    let err = convert(&www, true).unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");
    drop(held);
    assert_eq!(snapshot(&www), before);
    let (dirs, sensitive) = flags(&www);
    assert_eq!(sensitive, 0, "{dirs} dirs");
    no_leftovers(&www);
}

#[test]
fn cancellation_restores_the_tree() {
    let Some(base) = supported_tempdir() else {
        return;
    };
    let www = build_tree(base.path());
    for i in 0..300 {
        fs::write(
            www.join(format!("Loja/storage/f{i:03}.txt")),
            format!("{i}"),
        )
        .unwrap();
    }
    let before = snapshot(&www);
    let moved = AtomicU64::new(0);
    let err = convert_tree(
        cs().as_ref(),
        &www,
        true,
        &ConvertOptions::default(),
        &mut |p| {
            if p.phase == forge_core::fs::case::ConvertPhase::Moving {
                moved.store(p.done, Ordering::SeqCst);
            }
        },
        &|| moved.load(Ordering::SeqCst) >= 64,
    )
    .unwrap_err();
    assert_eq!(err.code(), "cancelled", "{err}");
    assert_eq!(snapshot(&www), before);
    assert_eq!(flags(&www).1, 0);
    no_leftovers(&www);
}

#[test]
fn interrupted_conversions_are_recovered() {
    let Some(base) = supported_tempdir() else {
        return;
    };
    let www = build_tree(base.path());
    let before = snapshot(&www);
    let tmp = www.with_file_name(format!("www{TMP_SUFFIX}"));
    let old = www.with_file_name(format!("www{OLD_SUFFIX}"));

    // Crash while moving: some files already in tmp.
    fs::create_dir_all(tmp.join("Loja/views")).unwrap();
    fs::rename(
        www.join("Loja/views/Header.php"),
        tmp.join("Loja/views/Header.php"),
    )
    .unwrap();
    fs::rename(www.join("index.html"), tmp.join("index.html")).unwrap();
    let out = recover(cs().as_ref(), &www, &mut |_| {}).unwrap();
    assert_eq!(out, RecoverOutcome::Restored);
    assert_eq!(snapshot(&www), before);
    no_leftovers(&www);

    // Crash between the two renames of the swap: content in tmp, skeleton in old.
    fs::create_dir(&tmp).unwrap();
    fs::rename(www.join("index.html"), tmp.join("index.html")).unwrap();
    fs::rename(&www, &old).unwrap();
    // convert_tree recovers before doing its own work
    let report = convert(&www, true).unwrap();
    assert!(report.recovered);
    assert_eq!(snapshot(&www), before);
    no_leftovers(&www);

    // Crash after the swap: only the empty skeleton is left.
    fs::create_dir_all(old.join("Loja/views")).unwrap();
    let out = recover(cs().as_ref(), &www, &mut |_| {}).unwrap();
    assert_eq!(out, RecoverOutcome::Completed { leftover: None });
    no_leftovers(&www);
    // ...unless something wrote into it: then it is kept, never deleted.
    fs::create_dir_all(old.join("Loja")).unwrap();
    fs::write(old.join("Loja/new.txt"), "written during the conversion").unwrap();
    let out = recover(cs().as_ref(), &www, &mut |_| {}).unwrap();
    assert!(matches!(
        out,
        RecoverOutcome::Completed { leftover: Some(_) }
    ));
    assert_eq!(
        fs::read_to_string(old.join("Loja/new.txt")).unwrap(),
        "written during the conversion"
    );
}

#[test]
fn refuses_links_files_and_volume_roots() {
    let Some(base) = supported_tempdir() else {
        return;
    };
    let target = base.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = base.path().join("link");
    platform::current()
        .link_fs()
        .create_junction(&link, &target)
        .unwrap();
    assert_eq!(convert(&link, true).unwrap_err().code(), "invalid_input");
    let file = base.path().join("f.txt");
    fs::write(&file, "x").unwrap();
    assert_eq!(convert(&file, true).unwrap_err().code(), "invalid_input");
    assert_eq!(
        convert(Path::new(r"C:\"), true).unwrap_err().code(),
        "invalid_input"
    );
    assert_eq!(
        convert(&base.path().join("missing"), true)
            .unwrap_err()
            .code(),
        "not_found"
    );
}

// ---- acceptance: a copy of C:\laragon\www -----------------------------------------------

/// Copies `src` into `dst` (files and folders; links are skipped, never followed).
/// Returns the number of links skipped.
fn copy_tree(src: &Path, dst: &Path) -> u64 {
    let mut skipped = 0;
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let meta = fs::symlink_metadata(entry.path()).unwrap();
        let to = dst.join(entry.file_name());
        if platform::is_reparse_point(&meta) {
            skipped += 1;
        } else if meta.is_dir() {
            skipped += copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
            // copied read-only files would make the temp dir cleanup fail
            let mut perms = fs::metadata(&to).unwrap().permissions();
            if perms.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                fs::set_permissions(&to, perms).unwrap();
            }
        }
    }
    skipped
}

/// Files and bytes under `root` (links not followed), plus the sorted file list.
fn count(root: &Path) -> (u64, u64, Vec<PathBuf>) {
    let mut files = Vec::new();
    let mut bytes = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if platform::is_reparse_point(&meta) {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len();
                files.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    files.sort();
    (files.len() as u64, bytes, files)
}

/// Size of `dir`, stopping as soon as it passes `cap` bytes.
fn size_capped(dir: &Path, cap: u64) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if platform::is_reparse_point(&meta) {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
                if total > cap {
                    return total;
                }
            }
        }
    }
    total
}

/// Acceptance of M1-E4-T6 (docs/15 §3.1): converting a copy of `C:\laragon\www` keeps
/// every file and byte (20 sampled hashes equal), makes `A.txt` and `a.txt` coexist, and
/// the revert gives back an insensitive tree. The source is only read. When the whole
/// folder is over 2 GiB only three projects are copied: `FORGEBOX_LARAGON_PROJECTS`
/// (comma separated) or `example-app,vue_proj,@translado` (Laravel with `vendor/`, Vue with
/// `node_modules/`, a plain PHP site: ~35k files, ~310 MB; measured on the dev machine, the
/// whole folder has 3.5 GB).
#[test]
#[ignore = "needs C:\\laragon\\www (read only) and copies it into %TEMP%; run manually"]
fn laragon_www_copy_roundtrip() {
    let source = Path::new(r"C:\laragon\www");
    assert!(source.is_dir(), "C:\\laragon\\www not found");
    let Some(base) = supported_tempdir() else {
        return;
    };
    let copy = base.path().join("www");
    const CAP: u64 = 2 * 1024 * 1024 * 1024;
    let total = size_capped(source, CAP);
    let mut skipped_links = 0;
    if total <= CAP {
        eprintln!("copying all of C:\\laragon\\www ({total} bytes)");
        skipped_links += copy_tree(source, &copy);
    } else {
        let projects = std::env::var("FORGEBOX_LARAGON_PROJECTS")
            .unwrap_or_else(|_| "example-app,vue_proj,@translado".to_owned());
        eprintln!("C:\\laragon\\www is over 2 GiB; copying {projects}");
        fs::create_dir(&copy).unwrap();
        for name in projects.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            skipped_links += copy_tree(&source.join(name), &copy.join(name));
        }
    }
    let (files, bytes, list) = count(&copy);
    eprintln!("copy: {files} files, {bytes} bytes, {skipped_links} links skipped");
    assert!(files > 0);
    // 20 files spread over the sorted list, hashed independently of the converter.
    let step = (list.len() / 20).max(1);
    let samples: Vec<(PathBuf, String)> = list
        .iter()
        .step_by(step)
        .take(20)
        .map(|rel| (rel.clone(), sha256(&copy.join(rel))))
        .collect();
    let (dirs, _) = flags(&copy);

    let started = std::time::Instant::now();
    let report = convert(&copy, true).unwrap();
    eprintln!(
        "converted in {:?}: {} dirs, {} files, {} links, {} bytes, {} sampled",
        started.elapsed(),
        report.stats.directories,
        report.stats.files,
        report.stats.links,
        report.stats.bytes,
        report.sampled
    );
    let (files_after, bytes_after, _) = count(&copy);
    assert_eq!((files_after, bytes_after), (files, bytes));
    for (rel, hash) in &samples {
        assert_eq!(&sha256(&copy.join(rel)), hash, "{}", rel.display());
    }
    assert_eq!(flags(&copy), (dirs, dirs), "every folder is case-sensitive");
    no_leftovers(&copy);

    // A.txt and a.txt coexist at the top and deep inside a project.
    let deep = list
        .iter()
        .filter_map(|rel| rel.parent())
        .max_by_key(|p| p.components().count())
        .map(|p| copy.join(p))
        .unwrap_or_else(|| copy.clone());
    for dir in [copy.clone(), deep.clone()] {
        fs::write(dir.join("A.txt"), "upper").unwrap();
        fs::write(dir.join("a.txt"), "lower").unwrap();
        assert_eq!(fs::read_to_string(dir.join("A.txt")).unwrap(), "upper");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "lower");
    }
    eprintln!(
        "A.txt + a.txt coexist in {} and {}",
        copy.display(),
        deep.display()
    );
    assert_eq!(convert(&copy, false).unwrap_err().code(), "conflict");
    for dir in [copy.clone(), deep] {
        fs::remove_file(dir.join("A.txt")).unwrap();
        fs::remove_file(dir.join("a.txt")).unwrap();
    }

    let started = std::time::Instant::now();
    let reverted = convert(&copy, false).unwrap();
    eprintln!("reverted in {:?}", started.elapsed());
    assert!(!reverted.enabled);
    assert_eq!(flags(&copy), (dirs, 0), "insensitive again");
    let (files_back, bytes_back, _) = count(&copy);
    assert_eq!((files_back, bytes_back), (files, bytes));
    for (rel, hash) in &samples {
        assert_eq!(&sha256(&copy.join(rel)), hash);
    }
    fs::write(copy.join("Z.txt"), "z").unwrap();
    assert!(copy.join("z.txt").exists());
    no_leftovers(&copy);
}
