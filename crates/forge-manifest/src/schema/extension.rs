//! `kind: Extension` (docs/04 §2): installed inside a version of another tool.

use std::collections::BTreeMap;

use forge_core::ids::ToolId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    ApiVersion, Apply, ExtensionKind, Platform, ToolKind, VersionScheme, is_default,
};
use super::tool::{
    ArchiveFormat, Config, Install, PostInstallAction, Source, StripRoot, ToolManifest,
    ToolMetadata, ToolSpec, Uninstall, Versioning,
};
use crate::version::{Constraint, Version};

/// A complete `kind: Extension` manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: ApiVersion,
    pub kind: ExtensionKind,
    pub metadata: ToolMetadata,
    pub spec: ExtensionSpec,
}

/// `spec` of an Extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionSpec {
    /// The tool this extension goes into (`php`).
    pub extends: ToolId,
    /// Resolved against the target tool version; the first matching entry wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compat: Vec<Compat>,
    pub source: Source,
    pub install: ExtensionInstall,
    #[serde(default, skip_serializing_if = "is_default")]
    pub config: Config,
    /// Lines written to enable/disable the extension in the target's config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle: Option<Toggle>,
}

/// `spec.compat[]`: which extension versions fit which target builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compat {
    /// Constraint on the version of the extended tool (`"~8.3"`).
    pub tool: String,
    /// Required variant values of the target build (`{ ts: nts, compiler: vs17 }`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variant: BTreeMap<String, String>,
    /// Constraint on the extension version (`"3.4.*"`).
    pub version: String,
}

/// `spec.install` of an Extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInstall {
    /// Default: inferred from the file name (a `.dll` is a single `file`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveFormat>,
    /// Only for archives. Default `auto`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub strip_root: StripRoot,
    /// Destination inside the target version: the file itself (single file) or the folder
    /// where the archive is extracted. Must start with `{{ paths.version }}`.
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_install: Vec<PostInstallAction>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub uninstall: Uninstall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_estimate_mb: Option<u32>,
}

/// `spec.toggle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Toggle {
    pub enable: String,
    pub disable: String,
    /// Default `restart`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub apply: Apply,
    /// Named presets of the extension's own quick settings (`config.quick_settings[].key`
    /// → value, written like a `default`) that turning the extension on in that mode
    /// stores (M2-E2-T9: `php.xdebug {mode: on|trigger}` picks `modes.on` /
    /// `modes.trigger`; `off` writes the `disable` line). The Extension of a pool tool that
    /// declares `modes` is the one `php.xdebug` drives.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modes: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
}

impl Toggle {
    /// The directive of the `enable` line (`zend_extension` of
    /// `zend_extension=php_xdebug.dll`), lowercased; empty when the line has no `=`.
    pub fn directive(&self) -> String {
        self.enable
            .trim()
            .trim_start_matches(';')
            .split_once('=')
            .map(|(d, _)| d.trim().to_ascii_lowercase())
            .unwrap_or_default()
    }
}

// ------------------------------------------------------------------ resolution (ADR-0043)

/// Variant key of an installed extension (`tool_versions.variant`) naming the tool it
/// was installed into (`php`). The three `target_*` keys are the link between an
/// extension row and its target version (ADR-0043): release variants of an Extension
/// may not use the [`TARGET_KEY_PREFIX`].
pub const TARGET_TOOL_KEY: &str = "target_tool";
/// Variant key with the `VersionId` of the target version.
pub const TARGET_ID_KEY: &str = "target_id";
/// Variant key with the folder name of the target version (`8.4.27-nts-vs17-x64`).
pub const TARGET_NAME_KEY: &str = "target_name";
/// Reserved prefix of the link keys.
pub const TARGET_KEY_PREFIX: &str = "target_";

/// An installed build of the extended tool, as compatibility rules see it.
#[derive(Debug, Clone, Copy)]
pub struct TargetBuild<'a> {
    /// Version scheme of the extended tool.
    pub scheme: VersionScheme,
    /// `8.4.27`.
    pub version: &'a str,
    /// Series under the extended tool's `series_key` (`8.4`).
    pub series: &'a str,
    /// Variant values of the build (`{ts: nts, compiler: vs17, arch: x64}`).
    pub variants: &'a BTreeMap<String, String>,
}

impl ExtensionSpec {
    /// Release variant key (and URL placeholder) holding the series of the target
    /// version: `<extends>_series` (`php_series`), `-` written as `_`.
    pub fn series_variant_key(&self) -> String {
        format!("{}_series", self.extends.as_str().replace('-', "_"))
    }

    /// The first `compat` entry whose `tool` constraint matches the target version and
    /// whose `variant` values all equal the target's (a key the target lacks does not
    /// match: a DLL built for another runtime does not load). Without `compat` every
    /// target is accepted with no extension version constraint (`Some(None)`); `None`
    /// means incompatible.
    pub fn compat_for(&self, target: &TargetBuild<'_>) -> Option<Option<&Compat>> {
        if self.compat.is_empty() {
            return Some(None);
        }
        let version = Version::parse(target.scheme, target.version).ok()?;
        self.compat
            .iter()
            .find(|c| {
                Constraint::parse(target.scheme, &c.tool).is_ok_and(|k| k.matches(&version))
                    && c.variant
                        .iter()
                        .all(|(key, value)| target.variants.get(key) == Some(value))
            })
            .map(Some)
    }

    /// Whether a release (its extension `version` and free-form `variants`) fits the
    /// target under `compat`: the version satisfies `compat.version`, the
    /// [`series_variant_key`](Self::series_variant_key) equals the target series and every
    /// other key the target build also has carries the same value (keys the target does
    /// not declare are not checked).
    pub fn release_fits(
        &self,
        compat: Option<&Compat>,
        version: &str,
        variants: &BTreeMap<String, String>,
        target: &TargetBuild<'_>,
    ) -> bool {
        if let Some(compat) = compat {
            let fits = Version::parse(VersionScheme::Semver, version)
                .ok()
                .zip(Constraint::parse(VersionScheme::Semver, &compat.version).ok())
                .is_some_and(|(v, c)| c.matches(&v));
            if !fits {
                return false;
            }
        }
        let series_key = self.series_variant_key();
        variants.iter().all(|(key, value)| {
            if *key == series_key {
                value == target.series
            } else {
                target.variants.get(key).is_none_or(|t| t == value)
            }
        })
    }

    /// What the extension supports, for error messages: one line per `compat` entry
    /// (`8.4 (nts, vs17)`), or the series of the curated releases without `compat`.
    pub fn compatible_series(&self) -> Vec<String> {
        if self.compat.is_empty() {
            let key = self.series_variant_key();
            let mut out: Vec<String> = self
                .source
                .releases
                .iter()
                .filter_map(|r| r.variants.get(&key).cloned())
                .collect();
            out.dedup();
            return out;
        }
        self.compat
            .iter()
            .map(|c| {
                let series = c.tool.trim_start_matches(['~', '^', '=']).trim();
                if c.variant.is_empty() {
                    series.to_owned()
                } else {
                    let values: Vec<&str> = c.variant.values().map(String::as_str).collect();
                    format!("{series} ({})", values.join(", "))
                }
            })
            .collect()
    }
}

impl ExtensionManifest {
    /// The extension as a release source: a `Tool` view with its metadata and `source`,
    /// `semver` versioning and no declared variants (release variants of an Extension are
    /// free-form, docs/04 §2), so the providers resolve and order its releases like any
    /// tool's. Never installed as a tool.
    pub fn release_view(&self) -> ToolManifest {
        ToolManifest {
            api_version: self.api_version,
            kind: ToolKind::Tool,
            metadata: self.metadata.clone(),
            spec: ToolSpec {
                platforms: vec![Platform::WindowsX64],
                runtime_deps: Vec::new(),
                depends_on: Vec::new(),
                suggests: Vec::new(),
                versioning: Versioning {
                    scheme: VersionScheme::Semver,
                    ..Versioning::default()
                },
                source: self.spec.source.clone(),
                install: Install {
                    archive: self.spec.install.archive,
                    strip_root: self.spec.install.strip_root,
                    current_link: false,
                    size_estimate_mb: self.spec.install.size_estimate_mb,
                    ..Install::default()
                },
                config: Config::default(),
                service: Default::default(),
                discover: None,
                update: Default::default(),
                ui: Default::default(),
                database: None,
                mail: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xdebug() -> ExtensionManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../registry/tools/xdebug.yaml"),
        )
        .unwrap();
        match crate::load_str(&text).unwrap() {
            crate::Manifest::Extension(m) => m,
            _ => unreachable!(),
        }
    }

    fn variants(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn target<'a>(
        version: &'a str,
        series: &'a str,
        variants: &'a BTreeMap<String, String>,
    ) -> TargetBuild<'a> {
        TargetBuild {
            scheme: VersionScheme::Semver,
            version,
            series,
            variants,
        }
    }

    #[test]
    fn compat_picks_the_entry_of_the_target_series_and_build() {
        let spec = xdebug().spec;
        assert_eq!(spec.series_variant_key(), "php_series");
        let vs17 = variants(&[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")]);
        let compat = spec.compat_for(&target("8.4.27", "8.4", &vs17)).unwrap();
        assert_eq!(compat.unwrap().tool, "~8.4");
        // 8.3 is built with vs16: a vs17 8.3 build has no compatible entry.
        let odd = variants(&[("ts", "nts"), ("compiler", "vs17")]);
        assert!(spec.compat_for(&target("8.3.36", "8.3", &odd)).is_none());
        // Thread-safe builds are not curated; a target without `compiler` never matches.
        let ts = variants(&[("ts", "ts"), ("compiler", "vs17")]);
        assert!(spec.compat_for(&target("8.4.27", "8.4", &ts)).is_none());
        let unknown = variants(&[("ts", "nts")]);
        assert!(
            spec.compat_for(&target("8.4.27", "8.4", &unknown))
                .is_none()
        );
        // Outside every series.
        let old = variants(&[("ts", "nts"), ("compiler", "vs16")]);
        assert!(spec.compat_for(&target("8.0.30", "8.0", &old)).is_none());
    }

    #[test]
    fn releases_fit_by_series_variant_and_compat_version() {
        let spec = xdebug().spec;
        let build = variants(&[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")]);
        let t = target("8.4.27", "8.4", &build);
        let compat = spec.compat_for(&t).unwrap();
        let for_84 = variants(&[("php_series", "8.4"), ("ts", "nts"), ("compiler", "vs17")]);
        let for_85 = variants(&[("php_series", "8.5"), ("ts", "nts"), ("compiler", "vs17")]);
        assert!(spec.release_fits(compat, "3.5.3", &for_84, &t));
        assert!(!spec.release_fits(compat, "3.5.3", &for_85, &t));
        assert!(
            !spec.release_fits(compat, "3.4.1", &for_84, &t),
            "3.5.* only"
        );
        // A key the target does not declare is not checked; one it declares is.
        let extra = variants(&[("php_series", "8.4"), ("flavor", "x")]);
        assert!(spec.release_fits(compat, "3.5.3", &extra, &t));
        let wrong = variants(&[("php_series", "8.4"), ("arch", "arm64")]);
        assert!(!spec.release_fits(compat, "3.5.3", &wrong, &t));
    }

    #[test]
    fn compatible_series_lists_the_compat_entries() {
        let spec = xdebug().spec;
        assert_eq!(
            spec.compatible_series(),
            [
                "8.5 (vs17, nts)",
                "8.4 (vs17, nts)",
                "8.3 (vs16, nts)",
                "8.2 (vs16, nts)",
                "8.1 (vs16, nts)"
            ]
        );
        let mut bare = spec.clone();
        bare.compat.clear();
        assert_eq!(
            bare.compatible_series(),
            ["8.5", "8.4", "8.3", "8.2", "8.1"]
        );
        let any = variants(&[]);
        assert_eq!(bare.compat_for(&target("7.4.1", "7.4", &any)), Some(None));
    }

    #[test]
    fn release_view_resolves_like_a_tool_without_variants() {
        let ext = xdebug();
        let view = ext.release_view();
        assert_eq!(view.metadata.id, ext.metadata.id);
        assert_eq!(view.spec.source, ext.spec.source);
        assert!(view.spec.versioning.variants.is_empty());
        assert_eq!(view.spec.versioning.scheme, VersionScheme::Semver);
        assert!(!view.spec.install.current_link);
        assert!(view.spec.install.post_install.is_empty());
    }

    #[test]
    fn release_variants_cannot_use_the_link_keys() {
        let mut ext = xdebug();
        assert!(crate::validate(&crate::Manifest::Extension(ext.clone())).is_empty());
        ext.spec.source.releases[0]
            .variants
            .insert(TARGET_ID_KEY.to_owned(), "x".to_owned());
        let issues = crate::validate(&crate::Manifest::Extension(ext));
        assert_eq!(issues.len(), 1, "{issues:#?}");
        assert_eq!(
            issues[0].field,
            "spec.source.releases[0].variants.target_id"
        );
    }

    #[test]
    fn toggle_modes_are_presets_of_the_extensions_quick_settings() {
        let ext = xdebug();
        let toggle = ext.spec.toggle.clone().unwrap();
        assert_eq!(toggle.directive(), "zend_extension");
        let modes: Vec<&str> = toggle.modes.keys().map(String::as_str).collect();
        assert_eq!(modes, ["on", "trigger"]);
        assert_eq!(toggle.modes["on"]["start_with_request"], "yes");
        assert_eq!(
            toggle.modes["trigger"]["mode"],
            serde_json::json!(["debug", "develop"])
        );
        // A preset naming a key the extension does not have, a bad mode name, an empty one.
        let mut bad = ext.clone();
        let modes = &mut bad.spec.toggle.as_mut().unwrap().modes;
        modes
            .get_mut("on")
            .unwrap()
            .insert("nope".into(), serde_json::json!(1));
        modes.insert("Bad".into(), BTreeMap::new());
        let issues = crate::validate(&crate::Manifest::Extension(bad));
        let fields: Vec<&str> = issues.iter().map(|i| i.field.as_str()).collect();
        assert_eq!(
            fields,
            [
                "spec.toggle.modes.Bad",
                "spec.toggle.modes.Bad",
                "spec.toggle.modes.on.nope"
            ],
            "{issues:#?}"
        );
    }

    #[test]
    fn extension_choice_keys_are_reserved_in_tools_with_extensions() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/php.yaml"),
        )
        .unwrap();
        let crate::Manifest::Tool(mut php) = crate::load_str(&text).unwrap() else {
            unreachable!()
        };
        assert!(crate::validate(&crate::Manifest::Tool(php.clone())).is_empty());
        php.spec.config.quick_settings[0].key = "ext_intl".into();
        let issues = crate::validate(&crate::Manifest::Tool(php));
        assert_eq!(issues.len(), 1, "{issues:#?}");
        assert_eq!(issues[0].field, "spec.config.quick_settings[0].key");
        let ext = crate::schema::tool::ExtensionsConfig {
            source: Default::default(),
            ini_dir: String::new(),
            pattern: "php_{name}.dll".into(),
            dir: "ext".into(),
            defaults_on: Vec::new(),
            zend_extensions: Vec::new(),
        };
        assert_eq!(ext.name_of("php_intl.dll").as_deref(), Some("intl"));
        assert_eq!(
            ext.name_of("PHP_PDO_MySQL.DLL").as_deref(),
            Some("pdo_mysql")
        );
        assert_eq!(ext.name_of("php_.dll"), None);
        assert_eq!(ext.name_of("libssl.dll"), None);
    }
}
