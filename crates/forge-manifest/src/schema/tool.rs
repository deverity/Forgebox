//! `kind: Tool` (docs/04 §1) and the blocks shared with `kind: Extension`
//! (`source`, `install.post_install`, `config`).

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::version::{Constraint, Version};
use forge_core::Timestamp;
use forge_core::ids::ToolId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::common::{
    ApiVersion, Apply, Category, Duration, Maintainer, Platform, RestartOn, RunAction, Sha256,
    ToolKind, VersionScheme, default_true, is_default, is_false, is_true,
};

/// A complete `kind: Tool` manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: ApiVersion,
    pub kind: ToolKind,
    pub metadata: ToolMetadata,
    pub spec: ToolSpec,
}

/// `metadata` of a Tool or Extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolMetadata {
    /// Unique slug in the registry (`[a-z0-9-]`).
    pub id: ToolId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Long Markdown text shown in the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: Category,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    /// SPDX expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// SVG file in `registry/icons/` or an `https://` URL (rule 7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintainers: Vec<Maintainer>,
    /// Hidden from the catalog (installed versions are kept).
    #[serde(default, skip_serializing_if = "is_false")]
    pub deprecated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<ToolId>,
}

/// `spec` of a Tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Must not be empty (rule 1).
    pub platforms: Vec<Platform>,
    /// System prerequisites known by the engine (`vcredist-vs17`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_deps: Vec<String>,
    /// Other tools that must be installed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<ToolId>,
    /// Offered by the UI after installing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggests: Vec<ToolId>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub versioning: Versioning,
    pub source: Source,
    #[serde(default, skip_serializing_if = "is_default")]
    pub install: Install,
    #[serde(default, skip_serializing_if = "is_default")]
    pub config: Config,
    #[serde(default, skip_serializing_if = "is_default")]
    pub service: Service,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discover: Option<Discover>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub update: Update,
    #[serde(default, skip_serializing_if = "is_default")]
    pub ui: Ui,
}

// ---------------------------------------------------------------- versioning

/// `spec.versioning`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Versioning {
    /// Default `semver`.
    pub scheme: VersionScheme,
    /// Named channels (`stable`, `preview`, `lts`...).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, Channel>,
    /// Groups versions into a series (`{major}`, `{minor}`, `{patch}`). Default `{major}.{minor}`.
    pub series_key: String,
    /// Orthogonal build dimensions, in the order used by the folder name (docs/08 §5).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,
}

/// Default of `versioning.series_key`.
pub const DEFAULT_SERIES_KEY: &str = "{major}.{minor}";

impl Default for Versioning {
    fn default() -> Self {
        Self {
            scheme: VersionScheme::Semver,
            channels: BTreeMap::new(),
            series_key: DEFAULT_SERIES_KEY.to_owned(),
            variants: Vec::new(),
        }
    }
}

impl Versioning {
    /// Variant keys in declaration order (the `order` of `version::canonical_name`).
    pub fn variant_order(&self) -> Vec<&str> {
        self.variants.iter().map(|v| v.key.as_str()).collect()
    }

    pub fn variant(&self, key: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.key == key)
    }
}

/// `versioning.channels.<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    /// Version constraint (`">=7.4 <9"`, `"~8.3"`), see `version::Constraint`.
    pub constraint: String,
    /// Whether pre-releases (`8.6.0-RC1`) belong to the channel. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prerelease: bool,
}

/// `versioning.variants[]`: one build dimension (`ts`, `compiler`, `arch`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub key: String,
    /// Values in order of preference (`[nts, ts]`, `[vs17, vs16, vc15]`).
    pub values: Vec<String>,
    /// Preferred value; ranks before the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Human labels per value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub label: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive: Option<VariantDerive>,
}

/// Where the value of a variant comes from when the user does not choose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VariantDerive {
    /// The provider reports it with each release.
    FromProvider,
}

// -------------------------------------------------------------------- source

/// `spec.source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Tried in order; the first that resolves the version wins.
    pub providers: Vec<Provider>,
    /// URL templates with `{file}` tried in order after the original URL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    /// Static list of releases (curated by the registry bot when upstream has no API/hash).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub releases: Vec<StaticRelease>,
}

/// `source.releases[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StaticRelease {
    pub version: String,
    /// Omitted when the provider builds the URL (`http` with `url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256>,
    /// Bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Variant values of this build (`{ ts: nts, compiler: vs17, arch: x64 }`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<Timestamp>,
}

/// `source.providers[]`, selected by `type` (docs/05 §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Provider {
    PhpWindows(PhpWindowsProvider),
    ApacheLounge(ApacheLoungeProvider),
    NginxOrg(NginxOrgProvider),
    GithubRelease(GithubReleaseProvider),
    Mysql(MysqlProvider),
    Mariadb(MariadbProvider),
    PostgresqlEdb(PostgresqlEdbProvider),
    NodejsDist(NodejsDistProvider),
    PythonEmbed(PythonEmbedProvider),
    Http(HttpProvider),
    Local(LocalProvider),
}

impl Provider {
    /// The `type` value.
    pub fn type_name(&self) -> &'static str {
        match self {
            Provider::PhpWindows(_) => "php-windows",
            Provider::ApacheLounge(_) => "apache-lounge",
            Provider::NginxOrg(_) => "nginx-org",
            Provider::GithubRelease(_) => "github-release",
            Provider::Mysql(_) => "mysql",
            Provider::Mariadb(_) => "mariadb",
            Provider::PostgresqlEdb(_) => "postgresql-edb",
            Provider::NodejsDist(_) => "nodejs-dist",
            Provider::PythonEmbed(_) => "python-embed",
            Provider::Http(_) => "http",
            Provider::Local(_) => "local",
        }
    }

    /// Whether the provider gets a SHA-256/512 from upstream (docs/05 §1). When it
    /// does not, every `source.releases[]` must carry a curated `sha256` (rule 2).
    pub fn has_upstream_checksum(&self) -> bool {
        match self {
            Provider::PhpWindows(_)
            | Provider::ApacheLounge(_)
            | Provider::Mariadb(_)
            | Provider::NodejsDist(_)
            | Provider::Local(_) => true,
            Provider::GithubRelease(p) => p.checksum.is_some(),
            Provider::Http(p) => p.checksum.is_some(),
            Provider::NginxOrg(_)
            | Provider::Mysql(_)
            | Provider::PostgresqlEdb(_)
            | Provider::PythonEmbed(_) => false,
        }
    }
}

/// `type: php-windows`: `releases.json` of windows.php.net.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhpWindowsProvider {
    /// Also list old versions from `releases/archives/`. Default `true`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub include_archives: bool,
    /// QA/RC builds (`qa/`), only for channels with `prerelease`. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub qa: bool,
}

/// `type: apache-lounge`: download pages of apachelounge.com.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApacheLoungeProvider {
    /// Pages to parse (`download/VS17/`, `download/VS18/`). Default `[vs17, vs18]`.
    #[serde(
        default = "default_apache_builds",
        skip_serializing_if = "is_default_apache_builds"
    )]
    pub builds: Vec<String>,
    /// Regex for the file name; default: the `httpd-<version>-<date>-win64-VS<nn>.zip` archive.
    /// Modules on the same page (mod_fcgid) set their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

fn default_apache_builds() -> Vec<String> {
    vec!["vs17".to_owned(), "vs18".to_owned()]
}

fn is_default_apache_builds(value: &Vec<String>) -> bool {
    *value == default_apache_builds()
}

/// `type: nginx-org`: `nginx.org/en/download.html` (mainline and stable). Upstream has no
/// hash (PGP only), so `releases[].sha256` is curated (rule 2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NginxOrgProvider {}

/// `type: github-release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubReleaseProvider {
    /// `owner/name`.
    pub repo: String,
    /// Regex matched against asset names (`^mailpit-windows-amd64\.zip$`).
    pub asset: String,
    /// Checksum asset; without it, `releases[].sha256` is required (rule 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<AssetChecksum>,
    /// Include GitHub pre-releases. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prerelease: bool,
    /// Regex with one group extracting the version from the tag. Default: tag without a leading `v`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_regex: Option<String>,
}

/// Checksum published as a release asset (`checksums.txt`, `{asset}.sha256`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetChecksum {
    #[serde(default, rename = "type", skip_serializing_if = "is_default")]
    pub kind: ChecksumType,
    /// Asset name; `{asset}` is replaced by the downloaded asset name.
    pub asset: String,
}

/// `type: mysql`: curated `releases:` (upstream publishes md5 only).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MysqlProvider {}

/// `type: mariadb`: official REST API (`downloads.mariadb.org/rest-api`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MariadbProvider {}

/// `type: postgresql-edb`: curated `releases:` (EDB has no index).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresqlEdbProvider {}

/// `type: nodejs-dist`: `nodejs.org/dist/index.json` + `SHASUMS256.txt`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodejsDistProvider {}

/// `type: python-embed`: python.org packages; hashes are curated (rule 2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PythonEmbedProvider {
    /// Default `embed`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub package: PythonPackage,
}

/// Which python.org package `python-embed` downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PythonPackage {
    /// `python-3.X.Y-embed-amd64.zip`.
    #[default]
    Embed,
    /// `python.3.X.Y.nupkg` (includes pip).
    Nuget,
}

/// `type: http`: generic URL template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpProvider {
    /// Placeholders: `{version}`, `{series}`, `{major}`, `{minor}`, `{patch}`, variant keys, `vars`.
    pub url: String,
    /// Without it, `releases[].sha256` is required (rule 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// Extra placeholders mapped from a variant value: `{ ts_flag: { nts: "nts", ts: "" } }`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, BTreeMap<String, String>>,
    /// Where to list versions when `releases:` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_source: Option<VersionSource>,
}

/// `checksum` of an `http` provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Checksum {
    #[serde(default, rename = "type", skip_serializing_if = "is_default")]
    pub kind: ChecksumType,
    /// URL template of the checksum file (`{url}`, `{file}` and the provider placeholders).
    pub url: String,
}

/// Hash algorithm. Default `sha256`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChecksumType {
    #[default]
    Sha256,
    Sha512,
}

/// `version_source` of an `http` provider: one of `regex` or `json_path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionSource {
    pub url: String,
    /// Regex with one capture group per version, applied to the body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// JSONPath to the list of versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
}

/// `type: local`: archive given by the user (`forge tool add-local <zip>`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalProvider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

// ------------------------------------------------------------------- install

/// `spec.install` of a Tool. Every field has a default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Install {
    /// Default: inferred from the file name (`ArchiveFormat::infer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveFormat>,
    /// Default `auto`.
    #[serde(skip_serializing_if = "is_default")]
    pub strip_root: StripRoot,
    /// Default `versioned`.
    #[serde(skip_serializing_if = "is_default")]
    pub layout: Layout,
    /// Create/update the `bin/<id>/current` junction. Default `true`.
    #[serde(skip_serializing_if = "is_true")]
    pub current_link: bool,
    /// Sub-folders exposed on PATH through `current` (`"."`). Default: none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// `bin/shims/<name>.cmd → bin/<id>/current/<name>.exe`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shims: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<Verify>,
    /// Declarative, idempotent actions run in order after extraction.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_install: Vec<PostInstallAction>,
    #[serde(skip_serializing_if = "is_default")]
    pub uninstall: Uninstall,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_estimate_mb: Option<u32>,
}

impl Default for Install {
    fn default() -> Self {
        Self {
            archive: None,
            strip_root: StripRoot::Auto,
            layout: Layout::Versioned,
            current_link: true,
            path: Vec::new(),
            shims: Vec::new(),
            verify: None,
            post_install: Vec::new(),
            uninstall: Uninstall::default(),
            size_estimate_mb: None,
        }
    }
}

/// Format of the downloaded file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ArchiveFormat {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "tar.xz")]
    TarXz,
    /// A single file copied as is (`.exe`, `.dll`, `.phar`).
    #[serde(rename = "file")]
    File,
    /// Not supported in v1 (validation error).
    #[serde(rename = "msi")]
    Msi,
}

impl ArchiveFormat {
    /// Format implied by a file name; anything unknown is a single [`ArchiveFormat::File`].
    pub fn infer(file_name: &str) -> Self {
        let name = file_name.to_ascii_lowercase();
        if name.ends_with(".zip") {
            Self::Zip
        } else if name.ends_with(".7z") {
            Self::SevenZ
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Self::TarGz
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Self::TarXz
        } else if name.ends_with(".msi") {
            Self::Msi
        } else {
            Self::File
        }
    }
}

/// `strip_root`: `true`, `false` or `auto` (remove a single top-level folder if present).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StripRoot {
    Yes,
    No,
    #[default]
    Auto,
}

impl Serialize for StripRoot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            StripRoot::Yes => serializer.serialize_bool(true),
            StripRoot::No => serializer.serialize_bool(false),
            StripRoot::Auto => serializer.serialize_str("auto"),
        }
    }
}

impl<'de> Deserialize<'de> for StripRoot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BoolOrWord::deserialize(deserializer)? {
            BoolOrWord::Bool(true) => Ok(StripRoot::Yes),
            BoolOrWord::Bool(false) => Ok(StripRoot::No),
            BoolOrWord::Word(w) if w == "auto" => Ok(StripRoot::Auto),
            BoolOrWord::Word(w) => Err(serde::de::Error::custom(format!(
                "invalid strip_root {w:?}: expected true, false or auto"
            ))),
        }
    }
}

impl JsonSchema for StripRoot {
    fn schema_name() -> Cow<'static, str> {
        "StripRoot".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "true, false or auto (remove a single top-level folder if present).",
            "oneOf": [{ "type": "boolean" }, { "const": "auto" }]
        })
    }
}

/// `true`/`false` or a keyword, for fields like `strip_root` and `autostart`.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum BoolOrWord {
    Bool(bool),
    Word(String),
}

/// `install.layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// `bin/<id>/<version>/` (default).
    #[default]
    Versioned,
    /// `bin/<id>/` (tools without versions side by side).
    Single,
}

/// `install.verify`: command run inside the version folder after install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Verify {
    pub command: String,
    /// Regex whose group 1 must equal the installed version. Default: exit code 0 is enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

/// `install.post_install[]`: one action, written as a single-key map (`- copy: {...}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostInstallAction {
    Copy(CopyAction),
    Render(RenderAction),
    /// Folder to create (inside the root: starts with `{{ paths.<dir> }}`).
    Mkdir(String),
    Run(RunAction),
}

/// `post_install[].copy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyAction {
    /// Relative to the version folder, or starting with `{{ paths.<dir> }}`.
    pub from: String,
    /// Must start with `{{ paths.<dir> }}` (rule 6).
    pub to: String,
    /// Keep an existing destination (user edits). Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub if_missing: bool,
}

/// `post_install[].render`: template from `forge-render/templates/` or `<registry>/templates/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderAction {
    /// Relative path with `/` (`php/php.ini.j2`); must exist (rule 4).
    pub template: String,
    /// Must start with `{{ paths.<dir> }}` (rule 6).
    pub to: String,
}

/// `install.uninstall`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Uninstall {
    /// Paths preserved when the version is removed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keep: Vec<String>,
}

// -------------------------------------------------------------------- config

/// `spec.config`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ConfigFile>,
    /// Native controls in the UI (docs/04 §1.3).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quick_settings: Vec<QuickSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionsConfig>,
    /// Variables injected in the processes of the tool.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// `config.files[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Referenced by `quick_settings[].file`, `ui.actions[].open: config:<id>` and
    /// `{{ instance.config.<id> }}`.
    pub id: String,
    pub path: String,
    pub format: ConfigFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Default `true`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub editable: bool,
}

/// Format of a config file; decides how quick settings are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigFormat {
    Ini,
    /// Apache/Nginx directives; edited only inside the managed block.
    ConfDirective,
    Yaml,
    Json,
    /// Command line arguments of the service.
    Args,
    /// Plain text: editable, no quick settings.
    Text,
}

/// `config.quick_settings[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuickSetting {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `config.files[].id` written by this setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Target in an `ini` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ini: Option<String>,
    /// Target in a `conf-directive` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive: Option<String>,
    /// Target in a `yaml`/`json` file (dotted path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Target in an `args` file (`--smtp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    #[serde(rename = "type")]
    pub kind: QuickSettingType,
    /// Choices of `select` and `multi`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<serde_json::Value>,
    /// Value written by a `toggle` when on/off. Default: the native boolean of the format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<String>,
    /// Bounds of `number`/`slider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Default `restart`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub apply: Apply,
    /// Other keys written with the same value (`{ post_max_size: "{{ value }}" }`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub also_set: BTreeMap<String, String>,
    /// `port`: reserve the port in the allocator (Xdebug 9003). Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reserve: bool,
}

/// `quick_settings[].type` (docs/04 §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QuickSettingType {
    Toggle,
    Select,
    Number,
    Text,
    Path,
    Port,
    /// Needs `min` and `max`; `step` optional.
    Slider,
    /// List of checkboxes over `options`.
    Multi,
}

/// `config.extensions`: bundled extensions shown as toggles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfig {
    /// Default `bundled`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub source: ExtensionSource,
    /// Where the generated `.ini` files go.
    pub ini_dir: String,
    /// File name of an extension with `{name}` (`php_{name}.dll`).
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defaults_on: Vec<String>,
    /// `zend_extension=` lines of the overlay (`opcache`), optionally gated by a version
    /// constraint (same syntax as `versioning.channels.*.constraint`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zend_extensions: Vec<ZendExtension>,
}

impl ExtensionsConfig {
    /// Names of the zend extensions that apply to `version` (items without a
    /// constraint always apply; an unparsable constraint never matches).
    pub fn zend_extensions_for(&self, scheme: VersionScheme, version: &Version) -> Vec<String> {
        self.zend_extensions
            .iter()
            .filter(|z| match z.constraint() {
                None => true,
                Some(c) => Constraint::parse(scheme, c).is_ok_and(|c| c.matches(version)),
            })
            .map(|z| z.name().to_owned())
            .collect()
    }
}

/// An item of `config.extensions.zend_extensions`: `opcache` or `{ name, constraint }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ZendExtension {
    /// Always emitted.
    Name(String),
    /// Emitted only when `constraint` matches the installed version.
    Gated(GatedZendExtension),
}

/// Object form of [`ZendExtension`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatedZendExtension {
    pub name: String,
    /// Version constraint in the tool scheme (`"<8.5"`); absent = always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
}

impl ZendExtension {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) => n,
            Self::Gated(g) => &g.name,
        }
    }

    pub fn constraint(&self) -> Option<&str> {
        match self {
            Self::Name(_) => None,
            Self::Gated(g) => g.constraint.as_deref(),
        }
    }
}

/// Where bundled extensions come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionSource {
    /// Files shipped in the tool archive (`ext/*.dll`).
    #[default]
    Bundled,
}

// ------------------------------------------------------------------- service

/// `spec.service`: `kind` plus the block of that kind (`daemon:`, `pool:`, `oneshot:`, `gui:`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Service {
    /// Default `none`.
    pub kind: ServiceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oneshot: Option<ProgramService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gui: Option<ProgramService>,
}

/// `service.kind` (docs/04 §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    /// Not a process (library, CLI).
    #[default]
    None,
    /// Long-running process managed by the supervisor.
    Daemon,
    /// FastCGI pool managed by `forge-fcgi`.
    Pool,
    /// Runs and exits.
    Oneshot,
    /// Graphical app launched on demand, outside the Job.
    Gui,
}

impl ServiceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ServiceKind::None => "none",
            ServiceKind::Daemon => "daemon",
            ServiceKind::Pool => "pool",
            ServiceKind::Oneshot => "oneshot",
            ServiceKind::Gui => "gui",
        }
    }
}

/// `service.daemon` (docs/04 §1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DaemonService {
    /// Several named instances with their own data dirs and ports. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multi_instance: bool,
    /// Relative to the version folder.
    pub exe: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Default `{{ paths.version }}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// `CREATE_NEW_PROCESS_GROUP`, needed for `ctrl-break`. Default `true`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub console_group: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PortSpec>,
    /// Config validation before start (`nginx -t`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CommandSpec>,
    /// Deadline of `Starting`. Default `30s`.
    #[serde(
        default = "default_start_timeout",
        skip_serializing_if = "is_default_start_timeout"
    )]
    pub start_timeout: Duration,
    /// Default: the process being alive is enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub stop: Stop,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload: Option<Reload>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub restart_policy: RestartPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<LogSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<DataSpec>,
    /// Tools whose service must be `Running` first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<ToolId>,
    /// Start with the daemon. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub autostart: bool,
}

fn default_start_timeout() -> Duration {
    Duration::from_secs(30)
}

fn is_default_start_timeout(value: &Duration) -> bool {
    *value == default_start_timeout()
}

/// `service.daemon.ports[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    /// Referenced as `{{ instance.ports.<name> }}`.
    pub name: String,
    /// Preferred port; default: first free port of `range`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<u16>,
    /// Environment variable that receives the port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Named range of `forgebox.toml` (`databases`, `web`...); must exist (rule 3).
    pub range: String,
}

/// A command line (`check`, `data.init`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub command: String,
}

/// `service.daemon.health`: exactly one of `tcp`, `http`, `command`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// `host:port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpProbe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandProbe>,
    /// Default `5s`.
    #[serde(
        default = "default_health_interval",
        skip_serializing_if = "is_default_health_interval"
    )]
    pub interval: Duration,
    /// Default `1`.
    #[serde(default = "default_one", skip_serializing_if = "is_one")]
    pub success_threshold: u32,
    /// Default `3`.
    #[serde(default = "default_three", skip_serializing_if = "is_three")]
    pub failure_threshold: u32,
}

fn default_health_interval() -> Duration {
    Duration::from_secs(5)
}

fn is_default_health_interval(value: &Duration) -> bool {
    *value == default_health_interval()
}

fn default_one() -> u32 {
    1
}

fn is_one(value: &u32) -> bool {
    *value == 1
}

fn default_three() -> u32 {
    3
}

fn is_three(value: &u32) -> bool {
    *value == 3
}

/// `health.http`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpProbe {
    pub url: String,
    /// Default `200`.
    #[serde(default = "default_status", skip_serializing_if = "is_default_status")]
    pub expect_status: u16,
}

fn default_status() -> u16 {
    200
}

fn is_default_status(value: &u16) -> bool {
    *value == 200
}

/// `health.command`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandProbe {
    pub cmd: String,
    /// Default `0`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub exit_code: i32,
}

/// `service.daemon.stop` (docs/03 §1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stop {
    /// First attempt (`mysqladmin shutdown`, `nginx -s quit`). Default: none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Budget of the command. Default `15s`.
    pub timeout: Duration,
    /// Next step if the command does not stop it. Default `ctrl-break`.
    pub then: StopSignal,
}

impl Default for Stop {
    fn default() -> Self {
        Self {
            command: None,
            timeout: Duration::from_secs(15),
            then: StopSignal::CtrlBreak,
        }
    }
}

/// Stop step after `stop.command`; the cascade always ends with `kill` (Job Object).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StopSignal {
    CtrlC,
    #[default]
    CtrlBreak,
    /// `WM_CLOSE` to the top-level windows (GUIs).
    Close,
    Kill,
}

/// `service.daemon.reload`: `command` or `signal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<ConsoleSignal>,
}

/// Console control event sent to the process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleSignal {
    CtrlC,
    CtrlBreak,
}

/// `service.daemon.restart_policy`. Default `{ on: failure, max: 3, window: 60s }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RestartPolicy {
    pub on: RestartOn,
    /// Restarts allowed inside `window`.
    pub max: u32,
    pub window: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            on: RestartOn::Failure,
            max: 3,
            window: Duration::from_secs(60),
        }
    }
}

/// `service.daemon.logs[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogSpec {
    pub label: String,
    pub path: String,
    /// Parser id for the log viewer (`mysql`, `nginx-error`, `json`). Default: plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// `service.daemon.data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataSpec {
    pub dir: String,
    /// Run once when `dir` does not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init: Option<CommandSpec>,
}

/// `service.pool`: FastCGI pool (docs/03 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolService {
    pub exe: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Default `fastcgi`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub protocol: PoolProtocol,
    /// Named range of `forgebox.toml`; must exist (rule 3). Default `php-pools`.
    #[serde(
        default = "default_pool_range",
        skip_serializing_if = "is_default_pool_range"
    )]
    pub port_range: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub scaling: Scaling,
    #[serde(default, skip_serializing_if = "is_default")]
    pub recycle: Recycle,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<PoolHealth>,
}

fn default_pool_range() -> String {
    "php-pools".to_owned()
}

fn is_default_pool_range(value: &String) -> bool {
    value == "php-pools"
}

/// Protocol spoken by the pool workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PoolProtocol {
    #[default]
    Fastcgi,
}

/// `pool.scaling`. Default `{ min: 2, max: 12, idle_timeout: 120s }` (docs/03 §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Scaling {
    pub min: u32,
    pub max: u32,
    pub idle_timeout: Duration,
}

impl Default for Scaling {
    fn default() -> Self {
        Self {
            min: 2,
            max: 12,
            idle_timeout: Duration::from_secs(120),
        }
    }
}

/// `pool.recycle`. Default `{ max_requests: 500, max_rss_mb: 256, max_age: 1h }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Recycle {
    pub max_requests: u32,
    pub max_rss_mb: u32,
    pub max_age: Duration,
}

impl Default for Recycle {
    fn default() -> Self {
        Self {
            max_requests: 500,
            max_rss_mb: 256,
            max_age: Duration::from_secs(3600),
        }
    }
}

/// `pool.health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolHealth {
    /// Script requested through FastCGI (`/_forgebox/health.php`).
    pub fastcgi: String,
}

/// `service.oneshot` / `service.gui`: a program launched by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgramService {
    /// Relative to the version folder.
    pub exe: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// `oneshot` only. Default: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
}

// ------------------------------------------------------------------ discover

/// `spec.discover`: recognizing manual installs in `bin/` (docs/03 §5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Discover {
    /// Files that must exist in the folder.
    pub match_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_command: Option<String>,
    /// Regex with group 1 = version, applied to the output. Default: trimmed output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_regex: Option<String>,
    /// Per variant key (`ts`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variant_detect: BTreeMap<String, VariantDetect>,
}

/// `discover.variant_detect.<key>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VariantDetect {
    pub command: String,
    /// Group 1 is looked up in `map`.
    pub regex: String,
    pub map: BTreeMap<String, String>,
}

// -------------------------------------------------------------------- update

/// `spec.update` (docs/05 §4). Default `{ policy: notify, retention: 1, check_interval: 6h }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Update {
    pub policy: UpdatePolicy,
    /// Previous versions kept per series for rollback.
    pub retention: u32,
    pub check_interval: Duration,
    /// What to carry from the old version to the new one in the same series.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub migrate: Vec<MigrateAction>,
}

impl Default for Update {
    fn default() -> Self {
        Self {
            policy: UpdatePolicy::Notify,
            retention: 1,
            check_interval: Duration::from_secs(6 * 3600),
            migrate: Vec::new(),
        }
    }
}

/// `update.policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePolicy {
    Manual,
    #[default]
    Notify,
    AutoPatch,
    AutoMinor,
    Auto,
}

/// `update.migrate[]`: single-key map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrateAction {
    CopyDir(CopyDirAction),
}

/// `migrate[].copy_dir`: paths relative to the old and new version folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyDirAction {
    pub from: String,
    pub to: String,
    /// Only files missing in the new version. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub only_missing: bool,
}

// ------------------------------------------------------------------------ ui

/// `spec.ui`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// `#RRGGBB`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<UiAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
}

/// `ui.actions[]`: exactly one of `open`, `shell`, `browse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiAction {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `config:<file id>` or a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellAction>,
    /// URL opened in the browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse: Option<String>,
}

/// `ui.actions[].shell`: terminal with this version first on PATH.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ShellAction {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path_prepend: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zend_extensions_follow_constraint() {
        let cfg: ExtensionsConfig = serde_yaml_ng::from_str(
            "ini_dir: x
pattern: 'php_{name}.dll'
zend_extensions: [{ name: opcache, constraint: '<8.5' }, xdebug]",
        )
        .unwrap();
        let v = |t: &str| Version::parse(VersionScheme::Semver, t).unwrap();
        assert_eq!(
            cfg.zend_extensions_for(VersionScheme::Semver, &v("8.4.1")),
            ["opcache", "xdebug"]
        );
        assert_eq!(
            cfg.zend_extensions_for(VersionScheme::Semver, &v("8.5.0")),
            ["xdebug"]
        );
    }

    #[test]
    fn archive_format_is_inferred_from_file_name() {
        assert_eq!(
            ArchiveFormat::infer("php-8.3.0-nts.zip"),
            ArchiveFormat::Zip
        );
        assert_eq!(ArchiveFormat::infer("x.TAR.GZ"), ArchiveFormat::TarGz);
        assert_eq!(ArchiveFormat::infer("x.txz"), ArchiveFormat::TarXz);
        assert_eq!(ArchiveFormat::infer("x.7z"), ArchiveFormat::SevenZ);
        assert_eq!(ArchiveFormat::infer("x.msi"), ArchiveFormat::Msi);
        assert_eq!(ArchiveFormat::infer("php_xdebug.dll"), ArchiveFormat::File);
        assert_eq!(ArchiveFormat::infer("composer.phar"), ArchiveFormat::File);
    }

    #[test]
    fn strip_root_accepts_bool_or_auto() {
        for (text, expected) in [
            ("true", StripRoot::Yes),
            ("false", StripRoot::No),
            ("\"auto\"", StripRoot::Auto),
        ] {
            let value: StripRoot = serde_json::from_str(text).unwrap();
            assert_eq!(value, expected);
            assert_eq!(serde_json::to_string(&value).unwrap(), text);
        }
        assert!(serde_json::from_str::<StripRoot>("\"sometimes\"").is_err());
    }

    #[test]
    fn upstream_checksum_table() {
        let http = |checksum: Option<Checksum>| {
            Provider::Http(HttpProvider {
                url: "https://x/{version}.zip".into(),
                checksum,
                vars: BTreeMap::new(),
                version_source: None,
            })
        };
        assert!(!http(None).has_upstream_checksum());
        assert!(
            http(Some(Checksum {
                kind: ChecksumType::Sha256,
                url: "{url}.sha256".into()
            }))
            .has_upstream_checksum()
        );
        assert!(!Provider::Mysql(MysqlProvider {}).has_upstream_checksum());
        assert!(Provider::NodejsDist(NodejsDistProvider {}).has_upstream_checksum());
        assert_eq!(
            Provider::NginxOrg(NginxOrgProvider {}).type_name(),
            "nginx-org"
        );
    }
}
