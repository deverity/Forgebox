//! `sites.*`: sites, proxies, parked folders, projects, processes, sharing and stacks
//! (docs/09 §site, docs/06). Sites are identified by ULID; `root`/`dir` paths are
//! attributes on the daemon's machine (ADR-0009, rule 2).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{CheckDto, SiteRef};
use super::services::ServiceState;
use forge_core::Timestamp;
use forge_core::ids::{SiteId, ToolId};

/// Kind of site (docs/06 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SiteKind {
    Php,
    Static,
    Proxy,
    App,
}

/// Per-site Xdebug setting (docs/06 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SiteXdebug {
    Inherit,
    Off,
    Trigger,
    Always,
}

/// PHP selection of a `php` site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SitePhpDto {
    /// Series followed (`8.3`), if the site pins a series.
    pub series: Option<String>,
    /// Exact version (`8.3.36`), if the site pins one.
    pub exact: Option<String>,
    /// Version currently serving the site.
    pub resolved: Option<String>,
    /// Pool upstream (`php_8_3`).
    pub upstream: Option<String>,
}

/// A process managed for a site (Procfile entry, docs/06 §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteProcessDto {
    pub site_id: SiteId,
    pub host: String,
    /// Procfile name (`web`, `queue`, `scheduler`).
    pub name: String,
    pub command: String,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub autostart: bool,
}

/// A site (vhost or proxy), docs/06 §1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SiteDto {
    pub id: SiteId,
    /// Derived from the host (`loja-test`).
    pub slug: String,
    /// Always lowercase (docs/15 §7).
    pub host: String,
    pub aliases: Vec<String>,
    pub kind: SiteKind,
    /// Document root (informative path on the daemon's machine, `/` separators).
    pub root: Option<String>,
    /// Framework driver (`laravel`, `wordpress`, `generic-php`...).
    pub driver: Option<String>,
    /// Front web server tool (`nginx`, `apache`, `caddy`...).
    pub server: Option<ToolId>,
    pub php: Option<SitePhpDto>,
    /// Proxy target (`http://127.0.0.1:3000`), proxy sites only.
    pub upstream: Option<String>,
    pub websocket: bool,
    pub secure: bool,
    pub http_redirect: bool,
    pub env: BTreeMap<String, String>,
    pub xdebug: SiteXdebug,
    pub parked: bool,
    pub enabled: bool,
    /// Document root is case-sensitive (docs/15).
    pub case_sensitive: bool,
    /// Main URL (`https://loja.test`).
    pub url: String,
    pub processes: Vec<SiteProcessDto>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Result of `sites.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SiteListDto {
    pub sites: Vec<SiteDto>,
}

/// Params of methods acting on one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteParams {
    pub site: SiteRef,
}

/// Params of `sites.add` (`forge site add`, `forge link`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteAddParams {
    /// Project folder on the daemon's machine.
    pub dir: String,
    /// Default: `<folder>.<tld>`.
    #[serde(default)]
    pub host: Option<String>,
    /// PHP series or exact version (`8.3`, `8.3.36`).
    #[serde(default)]
    pub php: Option<String>,
    #[serde(default)]
    pub server: Option<ToolId>,
    /// Default: `prefs.secure_by_default`.
    #[serde(default)]
    pub secure: Option<bool>,
    /// Default: detected.
    #[serde(default)]
    pub driver: Option<String>,
}

/// Params of `sites.remove` (`forge site remove`, `forge unlink`, `forge proxy remove`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteRemoveParams {
    pub site: SiteRef,
    /// Destructive: must be `true`. Never deletes the project files.
    #[serde(default)]
    pub confirm: bool,
}

/// Params of `sites.php` (live PHP switch, docs/06 §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SitePhpParams {
    pub site: SiteRef,
    pub php: String,
}

/// Params of `sites.secure` (`forge site secure|unsecure`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteSecureParams {
    pub site: SiteRef,
    pub secure: bool,
}

/// Params of `sites.server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteServerParams {
    pub site: SiteRef,
    pub server: ToolId,
}

/// Add or remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ListAction {
    Add,
    Remove,
}

/// Params of `sites.alias`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteAliasParams {
    pub site: SiteRef,
    pub action: ListAction,
    pub alias: String,
}

/// What `sites.open` opens on the daemon's machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SiteOpenTarget {
    Browser,
    Editor,
    Terminal,
    Explorer,
}

/// Params of `sites.open` (local only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteOpenParams {
    pub site: SiteRef,
    pub with: SiteOpenTarget,
}

/// Result of `sites.info` (diagnosis: DNS, certificate, pool, root, driver).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SiteInfoDto {
    pub site: SiteDto,
    pub checks: Vec<CheckDto>,
}

/// Params of `sites.park` and `sites.unpark`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ParkParams {
    pub dir: String,
    /// TLD for the sites of this folder (default `.test`); ignored by `unpark`.
    #[serde(default)]
    pub tld: Option<String>,
}

/// A parked folder: each subfolder becomes `<name>.<tld>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ParkedDirDto {
    pub dir: String,
    pub tld: String,
    pub sites: u32,
}

/// Result of `sites.parked`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ParkedListDto {
    pub dirs: Vec<ParkedDirDto>,
}

/// Params of `sites.proxy_add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProxyAddParams {
    pub host: String,
    /// `http://127.0.0.1:3000`.
    pub upstream: String,
    #[serde(default)]
    pub websocket: bool,
    #[serde(default)]
    pub secure: Option<bool>,
}

/// Params of `sites.up`/`sites.down` (`forge up|down`, `forgebox.yaml` kind Project).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProjectParams {
    /// Project folder on the daemon's machine.
    pub dir: String,
}

/// Result of the `sites.up` job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ProjectUpDto {
    pub sites: Vec<SiteDto>,
    pub processes: Vec<SiteProcessDto>,
}

/// Params of `sites.ps`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SitePsParams {
    /// One site, or all.
    #[serde(default)]
    pub site: Option<SiteRef>,
}

/// Result of `sites.ps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteProcessListDto {
    pub processes: Vec<SiteProcessDto>,
}

/// Lifecycle command for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProcessAction {
    Start,
    Stop,
    Restart,
}

/// Params of `sites.run` (`forge run loja.test web start`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteRunParams {
    pub site: SiteRef,
    pub process: String,
    pub action: ProcessAction,
}

/// Params of `sites.share`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteShareParams {
    pub site: SiteRef,
    /// Tunnel tool (`cloudflared`, `ngrok`, `expose`); default from preferences.
    #[serde(default)]
    pub provider: Option<ToolId>,
    /// Stop sharing instead.
    #[serde(default)]
    pub stop: bool,
}

/// Result of the `sites.share` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ShareDto {
    pub site_id: SiteId,
    pub provider: ToolId,
    /// Public URL while shared.
    pub url: Option<String>,
}

/// Params of `sites.new` (`forge new laravel loja`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteNewParams {
    /// Stack id (`laravel`, `wordpress`...).
    pub stack: String,
    pub name: String,
    #[serde(default)]
    pub php: Option<String>,
    /// Database tool (`mysql`, `mariadb`, `postgresql`).
    #[serde(default)]
    pub db: Option<ToolId>,
    /// Parent folder (default: `www/`).
    #[serde(default)]
    pub dir: Option<String>,
}
