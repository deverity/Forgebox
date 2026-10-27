//! Case Mismatch Finder (docs/15 §6) against real folders: the acceptance fixture of
//! M1-E4-T6 (`tests/fixtures/case/`: `Views/header.php` vs `views/Header.php`, `./Header`
//! vs `header.js`, `asset('img/Logo.png')` vs `logo.png`), fixing with every strategy,
//! PSR-4, manifests and the other checks. Fixtures are copied to a temp folder first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use forge_ipc::api::fs_parity::CaseLintReportDto;
use forge_sites::lint_case::{
    self, CaseIssue, FixOptions, FixStrategy, IssueCategory, LintOptions, LintReport, RefKind,
    StrategyChoice,
};
use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/case")
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Names in `dir` exactly as on disk.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn lint(root: &Path) -> LintReport {
    lint_case::lint(&LintOptions::new(root)).unwrap()
}

fn fix(root: &Path, options: &FixOptions) -> LintReport {
    lint_case::fix(&LintOptions::new(root), options, &mut |_| {}, &|| false).unwrap()
}

fn mismatches(report: &LintReport) -> Vec<&CaseIssue> {
    report.mismatches().collect()
}

fn issue<'r>(report: &'r LintReport, file: &str) -> &'r CaseIssue {
    report
        .issues
        .iter()
        .find(|i| i.file == file)
        .unwrap_or_else(|| panic!("no issue in {file}: {:#?}", report.issues))
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
                out.insert(rel, Vec::new());
            } else {
                out.insert(rel, std::fs::read(e.path()).unwrap());
            }
        }
    }
    out
}

#[test]
fn fixture_reports_the_three_mismatches_with_file_and_line() {
    let report = lint(&fixture());
    let found: Vec<(&str, u32, RefKind, &str, &str)> = mismatches(&report)
        .iter()
        .map(|i| {
            (
                i.file.as_str(),
                i.line,
                i.kind,
                i.reference.as_str(),
                i.actual.as_str(),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            (
                "app/Http/Controllers/HomeController.php",
                14,
                RefKind::PhpPath,
                "Views/header.php",
                "views/Header.php"
            ),
            (
                "resources/js/app.js",
                1,
                RefKind::JsImport,
                "./Header",
                "./header"
            ),
            (
                "resources/views/welcome.blade.php",
                9,
                RefKind::Asset,
                "img/Logo.png",
                "img/logo.png"
            ),
        ]
    );
    // Nothing else in the fixture is wrong.
    assert_eq!(report.issues.len(), 3, "{:#?}", report.issues);
    let php = issue(&report, "app/Http/Controllers/HomeController.php");
    assert_eq!(php.id, "app/Http/Controllers/HomeController.php:14:33");
    assert_eq!(php.target.as_deref(), Some("views/Header.php"));
    assert_eq!(php.expected.as_deref(), Some("Views/header.php"));
    assert_eq!(php.fixes, [FixStrategy::Rename, FixStrategy::Edit]);
    let js = issue(&report, "resources/js/app.js");
    assert_eq!(js.target.as_deref(), Some("resources/js/header.js"));
    assert_eq!(js.expected.as_deref(), Some("resources/js/Header.js"));
    assert!(report.files_scanned >= 6);
    assert!(report.references_checked >= 6);
    assert!(!report.root.contains('\\'));
}

#[test]
fn fix_renames_or_edits_and_a_new_scan_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("loja");
    copy_tree(&fixture(), &root);
    let before = snapshot(&root);

    // Preview: nothing written, a plan per item.
    let preview = fix(
        &root,
        &FixOptions {
            dry_run: true,
            ..FixOptions::default()
        },
    );
    assert!(preview.fix && preview.dry_run && preview.remaining.is_none());
    assert_eq!(snapshot(&root), before, "dry run writes nothing");
    let plan = |file: &str| issue(&preview, file).fix.clone().unwrap();
    let php = plan("app/Http/Controllers/HomeController.php");
    assert_eq!(php.strategy, FixStrategy::Edit, "a folder differs: edit");
    assert_eq!(
        php.before.as_deref(),
        Some("        require_once base_path('Views/header.php');")
    );
    assert_eq!(
        php.after.as_deref(),
        Some("        require_once base_path('views/Header.php');")
    );
    let js = plan("resources/js/app.js");
    assert_eq!(
        js.strategy,
        FixStrategy::Rename,
        "only the file name differs"
    );
    assert_eq!(
        (js.from.as_str(), js.to.as_str()),
        ("resources/js/header.js", "resources/js/Header.js")
    );
    let img = plan("resources/views/welcome.blade.php");
    assert_eq!(img.strategy, FixStrategy::Rename);
    assert_eq!(img.to, "public/img/Logo.png");

    let report = fix(&root, &FixOptions::default());
    assert_eq!(report.remaining, Some(0));
    assert!(report.mismatches().all(|i| i.fixed), "{:#?}", report.issues);
    assert_eq!(mismatches(&lint(&root)).len(), 0);
    let controller =
        std::fs::read_to_string(root.join("app/Http/Controllers/HomeController.php")).unwrap();
    assert!(controller.contains("require_once base_path('views/Header.php');"));
    assert!(names(&root.join("resources/js")).contains(&"Header.js".to_owned()));
    assert!(names(&root.join("public/img")).contains(&"Logo.png".to_owned()));
    // Untouched: the other lines and files.
    assert_eq!(
        std::fs::read(root.join("resources/js/app.js")).unwrap(),
        before["resources/js/app.js"]
    );
}

#[test]
fn on_a_case_sensitive_folder_the_mismatches_really_break_and_the_fix_repairs_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("loja");
    copy_tree(&fixture(), &root);
    let platform = forge_core::platform::current();
    let cs = platform.case_sensitive_fs();
    if !cs.is_supported(&root) {
        eprintln!("SKIPPED: case sensitivity unsupported here");
        return;
    }
    forge_core::fs::case::convert_tree(
        cs,
        &root,
        true,
        &forge_core::fs::case::ConvertOptions::default(),
        &mut |_| {},
        &|| false,
    )
    .unwrap();
    // As on Linux: the spellings used by the code do not open.
    assert!(!root.join("Views/header.php").exists());
    assert!(!root.join("resources/js/Header.js").exists());
    assert!(!root.join("public/img/Logo.png").exists());
    // Same findings on a case-sensitive folder.
    assert_eq!(mismatches(&lint(&root)).len(), 3);

    let report = fix(&root, &FixOptions::default());
    assert_eq!(report.remaining, Some(0));
    assert!(root.join("views/Header.php").exists());
    assert!(root.join("resources/js/Header.js").exists());
    assert!(root.join("public/img/Logo.png").exists());
    assert!(!root.join("public/img/logo.png").exists());
}

#[test]
fn strategies_overrides_and_conflicting_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "package.json", "{}\n");
    write(root, "js/header.js", "export default 1;\n");
    write(root, "js/app.js", "import a from './Header';\n");
    write(root, "js/other.js", "import b from './HEADER';\n");
    write(root, "lib/a.php", "<?php\n");
    write(
        root,
        "index.php",
        "<?php\nrequire __DIR__ . '/Lib/a.php';\n",
    );

    let report = fix(
        root,
        &FixOptions {
            dry_run: true,
            ..FixOptions::default()
        },
    );
    // Two references ask for different spellings of header.js: both edited.
    assert_eq!(
        issue(&report, "js/app.js").fix.as_ref().unwrap().strategy,
        FixStrategy::Edit
    );
    assert_eq!(
        issue(&report, "js/other.js").fix.as_ref().unwrap().strategy,
        FixStrategy::Edit
    );
    let php = issue(&report, "index.php");
    assert_eq!(php.kind, RefKind::PhpInclude);
    assert_eq!(php.fix.as_ref().unwrap().strategy, FixStrategy::Edit);

    // Per item: rename header.js to the spelling of app.js and the lib folder; other.js
    // follows the default (edit) and ends up spelled like the renamed file.
    let app_id = issue(&report, "js/app.js").id.clone();
    let php_id = php.id.clone();
    let done = fix(
        root,
        &FixOptions {
            overrides: BTreeMap::from([
                (app_id, FixStrategy::Rename),
                (php_id, FixStrategy::Rename),
            ]),
            ..FixOptions::default()
        },
    );
    assert_eq!(done.remaining, Some(0), "{:#?}", done.issues);
    assert!(names(&root.join("js")).contains(&"Header.js".to_owned()));
    assert!(names(root).contains(&"Lib".to_owned()));
    assert_eq!(
        std::fs::read_to_string(root.join("js/other.js")).unwrap(),
        "import b from './Header';\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("js/app.js")).unwrap(),
        "import a from './Header';\n"
    );

    // `only` limits the run; `strategy: edit` forces edits.
    write(root, "js/third.js", "import c from './HEADER';\n");
    write(root, "js/fourth.js", "import d from './hEADER';\n");
    let scan = lint(root);
    let third = issue(&scan, "js/third.js").id.clone();
    let done = fix(
        root,
        &FixOptions {
            strategy: StrategyChoice::Edit,
            only: Some([third].into()),
            ..FixOptions::default()
        },
    );
    assert_eq!(done.remaining, Some(1));
    assert_eq!(
        std::fs::read_to_string(root.join("js/third.js")).unwrap(),
        "import c from './Header';\n"
    );
}

#[test]
fn psr4_classes_and_manifest_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "composer.json",
        "{\n  \"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}},\n  \"bin\": [\"bin/Tool\"]\n}\n",
    );
    write(
        root,
        "app/models/user.php",
        "<?php\n\nnamespace App\\Models;\n\nclass User\n{\n}\n",
    );
    write(
        root,
        "app/models/Post.php",
        "<?php\nnamespace App\\Models;\nfinal class Post {}\n",
    );
    write(
        root,
        "app/Http/Kernel.php",
        "<?php\nnamespace App\\Http;\nclass Kernel {}\n",
    );
    write(root, "bin/tool", "#!/usr/bin/env php\n<?php\n");

    let report = lint(root);
    let psr4: Vec<_> = report
        .mismatches()
        .filter(|i| i.kind == RefKind::Psr4)
        .map(|i| {
            (
                i.file.as_str(),
                i.line,
                i.reference.as_str(),
                i.detail.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        psr4,
        vec![
            (
                "app/models/Post.php",
                3,
                "app/Models/Post.php",
                Some("App\\Models\\Post")
            ),
            (
                "app/models/user.php",
                5,
                "app/Models/User.php",
                Some("App\\Models\\User")
            ),
        ]
    );
    let bin = issue(&report, "composer.json");
    assert_eq!(bin.kind, RefKind::Composer);
    assert_eq!(
        (bin.reference.as_str(), bin.actual.as_str()),
        ("bin/Tool", "bin/tool")
    );
    assert_eq!(bin.line, 3);

    let done = fix(root, &FixOptions::default());
    assert_eq!(done.remaining, Some(0), "{:#?}", done.issues);
    assert_eq!(names(&root.join("app")), ["Http", "Models"]);
    assert_eq!(names(&root.join("app/Models")), ["Post.php", "User.php"]);
    // composer.json: the bin path is edited (the file has another reference: itself).
    let composer = std::fs::read_to_string(root.join("composer.json")).unwrap();
    assert!(
        composer.contains("\"bin/tool\"") || names(&root.join("bin")) == ["Tool"],
        "{composer}"
    );
}

#[test]
fn ignored_folders_dynamic_and_missing_references() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "vendor/acme/lib/Helper.php",
        "<?php require 'NOPE.php';\n",
    );
    write(root, "node_modules/x/index.js", "import y from './Y';\n");
    write(root, "node_modules/x/y.js", "\n");
    write(
        root,
        "index.php",
        "<?php\nrequire __DIR__ . '/Vendor/acme/lib/Helper.php';\nrequire \"pages/$page.php\";\nrequire 'missing/File.php';\n",
    );
    let report = lint(root);
    // vendor/ and node_modules/ are not scanned, but a reference into them is checked;
    // renaming inside vendor/ is never offered.
    let all: Vec<_> = report.issues.iter().map(|i| i.file.as_str()).collect();
    assert_eq!(all, ["index.php"], "{:#?}", report.issues);
    let i = &report.issues[0];
    assert_eq!(i.reference, "/Vendor/acme/lib/Helper.php");
    assert_eq!(i.fixes, [FixStrategy::Edit]);
}

#[test]
fn duplicates_reserved_names_env_keys_and_crlf_shebangs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    write(&root, ".env", "APP_KEY=a\nDB_HOST=x\napp_key=b\n");
    write(&root, "artisan", "#!/usr/bin/env php\r\n<?php\r\n");
    write(&root, "bin/ok", "#!/bin/sh\necho ok\n");
    // A device name can only be created through a verbatim path.
    let aux = PathBuf::from(format!(r"\\?\{}", root.join("aux.php").display()));
    std::fs::write(&aux, "<?php\n").unwrap();

    let cs_supported = {
        let platform = forge_core::platform::current();
        let cs = platform.case_sensitive_fs();
        let dir = root.join("cs");
        std::fs::create_dir(&dir).unwrap();
        cs.set(&dir, true).is_ok()
    };
    if cs_supported {
        write(&root, "cs/Readme.md", "a\n");
        write(&root, "cs/README.md", "b\n");
    }

    let report = lint(&root);
    let by_cat = |c: IssueCategory| -> Vec<&CaseIssue> {
        report.issues.iter().filter(|i| i.category == c).collect()
    };
    let env = by_cat(IssueCategory::EnvCaseDuplicate);
    assert_eq!(env.len(), 1);
    assert_eq!(
        (
            env[0].line,
            env[0].reference.as_str(),
            env[0].actual.as_str()
        ),
        (3, "app_key", "APP_KEY")
    );
    let crlf = by_cat(IssueCategory::CrlfShebang);
    assert_eq!(crlf.len(), 1);
    assert_eq!(crlf[0].file, "artisan");
    let reserved = by_cat(IssueCategory::ReservedName);
    assert_eq!(reserved.len(), 1);
    assert_eq!(reserved[0].file, "aux.php");
    assert_eq!(reserved[0].detail.as_deref(), Some("reserved_device"));
    let dups = by_cat(IssueCategory::CaseDuplicate);
    if cs_supported {
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].file, "cs/README.md");
        assert_eq!(dups[0].actual, "Readme.md");
    } else {
        assert!(dups.is_empty());
    }
    assert!(report.issues.iter().all(|i| i.fixes.is_empty()));
    std::fs::remove_file(&aux).unwrap();
}

/// The report is the wire form of `fs_parity.lint_case` (`CaseLintReportDto`): every
/// field survives a decode/encode through the API type.
#[test]
fn report_json_is_the_api_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("loja");
    copy_tree(&fixture(), &root);
    write(&root, ".env", "A=1\na=2\n");
    for report in [
        lint(&root),
        fix(
            &root,
            &FixOptions {
                dry_run: true,
                ..FixOptions::default()
            },
        ),
        fix(&root, &FixOptions::default()),
    ] {
        let json = serde_json::to_value(&report).unwrap();
        let api: CaseLintReportDto = serde_json::from_value(json.clone())
            .unwrap_or_else(|e| panic!("not a CaseLintReportDto: {e}\n{json:#}"));
        let back = serde_json::to_value(&api).unwrap();
        assert_eq!(back, json, "a field was lost or renamed");
        assert_eq!(json["v"], 1);
        let kinds: Vec<&Value> = json["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| &i["kind"])
            .collect();
        assert!(kinds.iter().all(|k| k.is_string()));
    }
}
