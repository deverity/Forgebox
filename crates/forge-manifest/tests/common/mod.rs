//! Helpers shared by the integration tests: fixtures and temporary registries.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use forge_manifest::lint::{LintContext, LintReport, RegistryDir, lint_registry};
use forge_manifest::schema::common::Kind;
use forge_manifest::{Manifest, load_str};

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `*.yaml` of a fixture folder, sorted.
pub fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    files.sort();
    files
}

/// Templates the tests pretend forge-render embeds.
pub fn context() -> LintContext {
    LintContext::new(
        vec!["php/php.ini.j2".to_owned(), "php/xdebug.ini.j2".to_owned()],
        Vec::new(),
    )
}

/// A minimal valid tool manifest.
pub fn stub_tool(id: &str) -> String {
    format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\n\
         metadata: {{ id: {id}, name: {id}, category: devtool }}\n\
         spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n"
    )
}

pub const TINY_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"/>\n";

/// A temporary registry folder.
pub struct TempRegistry {
    pub dir: tempfile::TempDir,
}

impl TempRegistry {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.root().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Puts a manifest in `tools/` or `stacks/` (by kind) as `<stem>.yaml`; text that does
    /// not parse goes to `tools/`. Returns the relative path.
    pub fn add_manifest(&self, stem: &str, text: &str) -> String {
        let folder = match load_str(text).map(|m| m.kind()) {
            Ok(Kind::Stack) => "stacks",
            _ => "tools",
        };
        let relative = format!("{folder}/{stem}.yaml");
        self.write(&relative, text);
        relative
    }

    /// Creates `icons/<icon>` for every relative SVG icon of the parsed manifests.
    /// `big_icon` gets `bytes` bytes instead of a tiny SVG.
    pub fn add_icons(&self, big_icon: Option<(&str, usize)>) {
        let files = RegistryDir::new(self.root()).manifest_files().unwrap();
        for file in files {
            let Ok(manifest) = load_str(&read(&file)) else {
                continue;
            };
            let Some(icon) = manifest.icon() else {
                continue;
            };
            if icon.contains("://") || !icon.ends_with(".svg") {
                continue;
            }
            let content = match big_icon {
                Some((name, bytes)) if name == icon => {
                    let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\">");
                    while svg.len() < bytes {
                        svg.push_str("<!-- padding -->");
                    }
                    svg.push_str("</svg>");
                    svg
                }
                _ => TINY_SVG.to_owned(),
            };
            self.write(&format!("icons/{icon}"), &content);
        }
    }

    pub fn lint(&self, ctx: &LintContext) -> LintReport {
        lint_registry(&RegistryDir::new(self.root()), ctx)
    }
}

/// Codes of the issues of a report.
pub fn codes(report: &LintReport) -> BTreeSet<String> {
    report
        .issues
        .iter()
        .map(|i| i.code.as_str().to_owned())
        .collect()
}

/// Kind of a parsed manifest.
pub fn kind(manifest: &Manifest) -> Kind {
    manifest.kind()
}
