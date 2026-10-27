//! Variables of manifest strings at install time (docs/04 §1.4): `{{ version }}`,
//! `{{ series }}`, `{{ variant.<key> }}` and `{{ paths.<dir> }}`, all POSIX strings.
//!
//! Two sets exist during an installation ([`super::actions`]): the *physical* one, where
//! `paths.version` is the staging folder that holds the files right now (copy sources,
//! write targets inside the version, `run` executables and their arguments), and the
//! *final* one, where `paths.version` is `bin/<tool>/<canonical_name>` (what rendered
//! configuration must reference once the folder has been moved into place).

use std::collections::BTreeMap;
use std::path::Path;

use forge_core::error::{ForgeError, Result};
use forge_core::paths::{ForgeRoot, posix_string};
use forge_manifest::template;
use serde::{Deserialize, Serialize};

/// `paths.*`, POSIX strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathVars {
    pub root: String,
    pub bin: String,
    pub etc: String,
    pub data: String,
    pub logs: String,
    pub tmp: String,
    pub www: String,
    pub certs: String,
    /// Folder of the version being installed.
    pub version: String,
}

impl PathVars {
    /// Paths of `root` with `paths.version = version_dir`.
    pub fn new(root: &ForgeRoot, version_dir: &Path) -> Self {
        let p = |path: std::path::PathBuf| posix_string(&path);
        Self {
            root: root.to_posix_string(),
            bin: p(root.bin()),
            etc: p(root.etc()),
            data: p(root.data()),
            logs: p(root.logs()),
            tmp: p(root.tmp()),
            www: p(root.www()),
            certs: p(root.certs()),
            version: posix_string(version_dir),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        Some(match key {
            "root" => &self.root,
            "bin" => &self.bin,
            "etc" => &self.etc,
            "data" => &self.data,
            "logs" => &self.logs,
            "tmp" => &self.tmp,
            "www" => &self.www,
            "certs" => &self.certs,
            "version" => &self.version,
            _ => return None,
        })
    }
}

/// Everything a manifest string or an install-time template can reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVars {
    /// Tool id (`php`).
    pub tool: String,
    /// Exact version (`8.3.37`).
    pub version: String,
    /// Series under `versioning.series_key` (`8.3`); the version when it cannot be parsed.
    pub series: String,
    /// Folder name (`8.3.37-nts-vs17-x64`).
    pub canonical_name: String,
    /// Variant values of the build.
    pub variant: BTreeMap<String, String>,
    pub paths: PathVars,
}

impl TemplateVars {
    /// The same variables with `paths.version` pointing at `dir`.
    pub fn with_version_dir(&self, dir: &Path) -> Self {
        let mut out = self.clone();
        out.paths.version = posix_string(dir);
        out
    }

    fn lookup(&self, segments: &[String]) -> std::result::Result<String, String> {
        let unknown = || {
            Err(format!(
                "`{}` is not available at install time (version, series, variant.<key>, \
                 paths.<dir>)",
                segments.join(".")
            ))
        };
        match segments {
            [one] if one == "version" => Ok(self.version.clone()),
            [one] if one == "series" => Ok(self.series.clone()),
            [variant, key] if variant == "variant" => match self.variant.get(key) {
                Some(value) => Ok(value.clone()),
                None => Err(format!("this build has no variant `{key}`")),
            },
            [paths, key] if paths == "paths" => match self.paths.get(key) {
                Some(value) => Ok(value.to_owned()),
                None => unknown(),
            },
            _ => unknown(),
        }
    }

    /// Replaces every `{{ var }}` / `{{ var | filter }}` of `text`. Filters: `posix_path`
    /// (values already are) and `win_path` (`/` → `\`).
    pub fn substitute(&self, text: &str) -> Result<String> {
        let invalid = |why: String| ForgeError::InvalidInput(format!("{text:?}: {why}"));
        let refs = template::scan(text);
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        for found in refs {
            let var = found.map_err(invalid)?;
            let start = rest
                .find("{{")
                .ok_or_else(|| invalid("unbalanced template".into()))?;
            let end = rest[start..]
                .find("}}")
                .map(|e| start + e + 2)
                .ok_or_else(|| invalid("unclosed `{{`".into()))?;
            out.push_str(&rest[..start]);
            let mut value = self.lookup(&var.segments).map_err(invalid)?;
            for filter in var.expr.split('|').skip(1).map(str::trim) {
                match filter {
                    "posix_path" => value = value.replace('\\', "/"),
                    "win_path" => value = value.replace('/', "\\"),
                    other => return Err(invalid(format!("unknown filter `{other}`"))),
                }
            }
            out.push_str(&value);
            rest = &rest[end..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> TemplateVars {
        let root = ForgeRoot::new(r"C:\fb").unwrap();
        TemplateVars {
            tool: "php".into(),
            version: "8.3.37".into(),
            series: "8.3".into(),
            canonical_name: "8.3.37-nts".into(),
            variant: BTreeMap::from([("ts".into(), "nts".into())]),
            paths: PathVars::new(&root, &root.bin().join("php").join("8.3.37-nts")),
        }
    }

    #[test]
    fn substitutes_variables_and_filters() {
        let v = vars();
        assert_eq!(
            v.substitute("{{ paths.etc }}/php/{{series}}/php.ini")
                .unwrap(),
            "C:/fb/etc/php/8.3/php.ini"
        );
        assert_eq!(
            v.substitute("{{ paths.version | win_path }}\\ext {{ variant.ts }}")
                .unwrap(),
            r"C:\fb\bin\php\8.3.37-nts\ext nts"
        );
        assert_eq!(v.substitute("plain").unwrap(), "plain");
        let staged = v.with_version_dir(Path::new(r"C:\fb\tmp\stage\x\8.3.37-nts"));
        assert_eq!(
            staged.substitute("{{ paths.version }}").unwrap(),
            "C:/fb/tmp/stage/x/8.3.37-nts"
        );
    }

    #[test]
    fn rejects_what_install_time_cannot_know() {
        let v = vars();
        for bad in [
            "{{ instance.name }}",
            "{{ prefs.tld }}",
            "{{ variant.compiler }}",
            "{{ paths.nope }}",
            "{{ version | upper }}",
            "{{ 1 + 1 }}",
        ] {
            let err = v.substitute(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad}");
        }
    }
}
