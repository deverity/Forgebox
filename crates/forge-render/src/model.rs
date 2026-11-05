//! Neutral models that the templates consume (docs/03 §3.3, docs/06 §1.2).
//!
//! `forge-sites` drivers produce a [`VhostModel`]; this crate translates it to
//! Nginx/Apache/Caddy through templates. Nothing here knows about a specific server.
//! All path strings are POSIX (`/`) and hosts are lowercase (docs/15 §7).
//!
//! Values reach config files, so [`VhostModel::normalized`] validates them by position:
//! quoted values (env, header values, `cache_control`, root, ssl paths) reject `"`,
//! control characters (newlines) and a trailing `\` (which would escape the closing
//! quote); `$` and `%{` are also rejected where the server interpolates. Unquoted
//! tokens (index, try_files, deny, static patterns, `client_max_body`, upstream host)
//! additionally reject whitespace, quotes, `;`, `{` and `}`. Quoted values may contain
//! `;`, `{`, `}` because they stay inside the quotes. The only escape hatch is
//! [`ExtraModel`], which is raw by design and written by the user.

use std::collections::BTreeMap;
use std::path::Path;

use forge_core::error::{ForgeError, Result};
use forge_core::paths::{ForgeRoot, posix_string};
use serde::{Deserialize, Serialize};

/// Kind of site (docs/06 §1). `App` is a proxy to a process managed by Forgebox and is
/// rendered like `Proxy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteKind {
    /// PHP through an FPM-less FastCGI pool.
    #[default]
    Php,
    /// Plain files.
    Static,
    /// Reverse proxy to another process.
    Proxy,
    /// Proxy to a Procfile process of the site.
    App,
}

impl SiteKind {
    /// Which template family renders this kind: `php`, `static` or `proxy`.
    pub fn template_kind(self) -> &'static str {
        match self {
            Self::Php => "php",
            Self::Static => "static",
            Self::Proxy | Self::App => "proxy",
        }
    }
}

/// TLS material of a site (certificate files are issued by `forge-tls`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslModel {
    /// PEM certificate chain, POSIX path.
    pub cert: String,
    /// PEM private key, POSIX path.
    pub key: String,
}

/// A location matched by regex (case-sensitive: `~`, never `~*`, docs/15 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticLocation {
    /// Regex, e.g. `\.(?:css|js|png)$`. No whitespace, quotes, `;` or braces.
    pub pattern: String,
    /// Optional `Cache-Control` value for matching files.
    #[serde(default)]
    pub cache_control: Option<String>,
}

/// Reverse proxy settings (docs/06 §4.1).
///
/// Connections to the upstream are kept alive and reused (docs/06 §4.1.1): a new TCP
/// connection per request costs a Windows timer tick (~15.6 ms) on part of the connects
/// and stalls nginx's event loop for every site it serves. `keepalive_conns` and
/// `keepalive_idle_secs` are server-neutral: nginx `keepalive` / `keepalive_timeout` of
/// the site's `upstream {}`, Apache `ttl=` of the `ProxyPass` worker (`disablereuse=On`
/// when off), Caddy `keepalive` / `keepalive_idle_conns_per_host`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyModel {
    /// `http://127.0.0.1:3000` (no trailing slash, no path).
    pub upstream: String,
    /// Forward WebSocket upgrades (`forge proxy add --ws`).
    #[serde(default)]
    pub websocket: bool,
    /// Read timeout in seconds; long by default for HMR/WebSocket.
    #[serde(default = "default_read_timeout")]
    pub read_timeout_secs: u32,
    /// Show the "starting" page on 502 while an on-visit app boots (docs/06 §8).
    #[serde(default)]
    pub start_on_visit: bool,
    /// Idle upstream connections each server process keeps for reuse; 0 = a new
    /// connection per request (`Connection: close`, the behaviour before M2-E3-T22).
    #[serde(default = "default_keepalive_conns")]
    pub keepalive_conns: u32,
    /// Seconds an idle upstream connection is kept. Below the idle timeout of the common
    /// dev servers (Node and uvicorn close after 5 s), so the proxy never reuses a
    /// connection the app is closing right then (a POST on it would get a 502).
    #[serde(default = "default_keepalive_idle")]
    pub keepalive_idle_secs: u32,
    /// Derived by [`VhostModel::normalized`]: `http` or `https`.
    #[serde(default, skip_deserializing)]
    pub scheme: String,
    /// Derived: `host:port` of the upstream with the scheme's default port filled in
    /// (IPv6 keeps its brackets: `[::1]:5173`).
    #[serde(default, skip_deserializing)]
    pub address: String,
    /// Derived: the upstream host without brackets (TLS server name of `https` upstreams).
    #[serde(default, skip_deserializing)]
    pub host: String,
}

fn default_read_timeout() -> u32 {
    3600
}

/// Default of [`ProxyModel::keepalive_conns`]: enough idle connections for a burst of a
/// browser's parallel requests and HMR without keeping many sockets open per app.
pub const DEFAULT_PROXY_KEEPALIVE_CONNS: u32 = 32;
/// Default of [`ProxyModel::keepalive_idle_secs`].
pub const DEFAULT_PROXY_KEEPALIVE_IDLE_SECS: u32 = 4;

fn default_keepalive_conns() -> u32 {
    DEFAULT_PROXY_KEEPALIVE_CONNS
}

fn default_keepalive_idle() -> u32 {
    DEFAULT_PROXY_KEEPALIVE_IDLE_SECS
}

impl ProxyModel {
    /// Proxy to `upstream` with the defaults: no WebSocket, 1 h read timeout, upstream
    /// keep-alive on.
    pub fn new(upstream: impl Into<String>) -> Self {
        Self {
            upstream: upstream.into(),
            websocket: false,
            read_timeout_secs: default_read_timeout(),
            start_on_visit: false,
            keepalive_conns: DEFAULT_PROXY_KEEPALIVE_CONNS,
            keepalive_idle_secs: DEFAULT_PROXY_KEEPALIVE_IDLE_SECS,
            scheme: String::new(),
            address: String::new(),
            host: String::new(),
        }
    }

    /// Trims the trailing `/`, validates the upstream and fills `scheme`, `address` and
    /// `host`.
    fn normalize(&mut self) -> Result<()> {
        self.upstream = self.upstream.trim_end_matches('/').to_owned();
        check_token("proxy upstream", &self.upstream, "@?#")?;
        let upstream = self.upstream.clone();
        let (scheme, authority) = upstream
            .split_once("://")
            .filter(|(scheme, _)| matches!(*scheme, "http" | "https"))
            .ok_or_else(|| {
                invalid(format!(
                    "proxy upstream must be http:// or https://: '{upstream}'"
                ))
            })?;
        if authority.contains('/') {
            return Err(invalid(format!(
                "proxy upstream must not have a path: '{upstream}'"
            )));
        }
        let bad = || invalid(format!("invalid proxy upstream: '{upstream}'"));
        let (host, plain, port) = match authority.strip_prefix('[') {
            Some(rest) => {
                let (ip, after) = rest.split_once(']').ok_or_else(bad)?;
                if ip.is_empty() || !ip.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
                    return Err(bad());
                }
                let port = match after {
                    "" => None,
                    p => Some(p.strip_prefix(':').ok_or_else(bad)?),
                };
                (format!("[{ip}]"), ip.to_owned(), port)
            }
            None => {
                let (host, port) = match authority.split_once(':') {
                    Some((host, port)) => (host, Some(port)),
                    None => (authority, None),
                };
                if host.contains('*') || check_host(host).is_err() {
                    return Err(bad());
                }
                (host.to_owned(), host.to_owned(), port)
            }
        };
        let port = match port {
            Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(bad)?,
            None if scheme == "https" => 443,
            None => 80,
        };
        if self.read_timeout_secs == 0 {
            return Err(invalid("proxy read timeout must be positive"));
        }
        if self.keepalive_conns > 0 && self.keepalive_idle_secs == 0 {
            return Err(invalid(
                "proxy keep-alive idle time must be positive when keep-alive is on",
            ));
        }
        self.scheme = scheme.to_owned();
        self.address = format!("{host}:{port}");
        self.host = plain;
        Ok(())
    }
}

/// Per-server hand-written snippets inserted verbatim at the end of the site.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraModel {
    /// Raw Apache directives.
    #[serde(default)]
    pub apache: Option<String>,
    /// Raw Nginx directives.
    #[serde(default)]
    pub nginx: Option<String>,
    /// Raw Caddy directives.
    #[serde(default)]
    pub caddy: Option<String>,
}

/// Server-neutral description of one virtual host (docs/06 §1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VhostModel {
    /// Kind of site.
    pub kind: SiteKind,
    /// Hostnames; the first one is primary, must not be a wildcard and names the files.
    pub hosts: Vec<String>,
    /// Document root, POSIX path (php/static).
    pub root: String,
    /// `DirectoryIndex` list. Empty = default for the kind.
    pub index: Vec<String>,
    /// Name of the PHP pool upstream (`php_8_3`), php sites only.
    pub php_upstream: Option<String>,
    /// Nginx-style `try_files`. Empty = default for the kind.
    pub try_files: Vec<String>,
    /// Extra regex locations.
    pub static_locations: Vec<StaticLocation>,
    /// Regexes of paths answered with 403 (`/\.ht`, `/\.env`).
    pub deny: Vec<String>,
    /// Response headers added to every response.
    pub headers: BTreeMap<String, String>,
    /// Upload limit (`256M`).
    pub client_max_body: String,
    /// TLS material; `None` = plain HTTP site.
    pub ssl: Option<SslModel>,
    /// Redirect HTTP to HTTPS (only meaningful with `ssl`).
    pub http_redirect: bool,
    /// Proxy settings (proxy/app sites).
    pub proxy: Option<ProxyModel>,
    /// Extra FastCGI params / headers (`APP_ENV`).
    pub env: BTreeMap<String, String>,
    /// Escape hatch per server.
    pub extra: ExtraModel,
    /// Derived by [`VhostModel::normalized`] from the last `try_files` entry
    /// (`/index.php?...` becomes `/index.php`); used by Apache's `FallbackResource`.
    #[serde(skip_deserializing)]
    pub fallback_resource: Option<String>,
}

impl Default for VhostModel {
    fn default() -> Self {
        Self {
            kind: SiteKind::Php,
            hosts: Vec::new(),
            root: String::new(),
            index: Vec::new(),
            php_upstream: None,
            try_files: Vec::new(),
            static_locations: Vec::new(),
            deny: vec![r"/\.ht".to_owned(), r"/\.env".to_owned()],
            headers: BTreeMap::new(),
            client_max_body: "256M".to_owned(),
            ssl: None,
            http_redirect: false,
            proxy: None,
            env: BTreeMap::new(),
            extra: ExtraModel::default(),
            fallback_resource: None,
        }
    }
}

fn invalid(msg: impl Into<String>) -> ForgeError {
    ForgeError::InvalidInput(msg.into())
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Token that is safe both inside and outside quotes in every supported server syntax.
fn check_token(field: &str, value: &str, extra_forbidden: &str) -> Result<()> {
    let bad = value.is_empty()
        || has_control(value)
        || value.ends_with('\\')
        || value.chars().any(|c| {
            c.is_whitespace()
                || matches!(c, '"' | '\'' | ';' | '{' | '}')
                || extra_forbidden.contains(c)
        });
    if bad {
        return Err(invalid(format!("invalid {field}: '{value}'")));
    }
    Ok(())
}

/// Like [`check_text`] for values that nginx/Apache would interpolate: `$name` (nginx
/// variables, Apache back-references) and `%{...}` (Apache expressions) are rejected.
fn check_literal(field: &str, value: &str) -> Result<()> {
    check_text(field, value)?;
    if value.contains('$') || value.contains("%{") {
        return Err(invalid(format!(
            "invalid {field}: '$' and '%{{' are interpolated by the web server"
        )));
    }
    Ok(())
}

fn check_text(field: &str, value: &str) -> Result<()> {
    // A trailing backslash would escape the closing quote in nginx, Apache and Caddy.
    if value.is_empty() || has_control(value) || value.contains('"') || value.ends_with('\\') {
        return Err(invalid(format!("invalid {field}: '{value}'")));
    }
    Ok(())
}

fn check_host(host: &str) -> Result<()> {
    let ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '*'));
    if !ok {
        return Err(invalid(format!("invalid host: '{host}'")));
    }
    Ok(())
}

fn check_ident(field: &str, value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if !ok {
        return Err(invalid(format!("invalid {field}: '{value}'")));
    }
    Ok(())
}

impl VhostModel {
    /// Returns a validated copy with defaults applied: hosts lowercased, paths POSIX,
    /// `index`/`try_files` filled per kind, `fallback_resource` derived.
    pub fn normalized(&self) -> Result<Self> {
        let mut m = self.clone();
        if m.hosts.is_empty() {
            return Err(invalid("a site needs at least one host"));
        }
        for h in &mut m.hosts {
            *h = h.to_ascii_lowercase();
            check_host(h)?;
        }
        if m.hosts[0].contains('*') {
            return Err(invalid("the primary host cannot be a wildcard"));
        }
        let mut seen = std::collections::BTreeSet::new();
        m.hosts.retain(|h| seen.insert(h.clone()));

        let needs_root = matches!(m.kind, SiteKind::Php | SiteKind::Static);
        if needs_root {
            m.root = posix_string(Path::new(&m.root));
            check_text("root", &m.root)?;
        }
        match m.kind {
            SiteKind::Php => {
                let up = m
                    .php_upstream
                    .as_deref()
                    .ok_or_else(|| invalid("php site without php_upstream"))?;
                check_ident("php_upstream", up)?;
                if m.index.is_empty() {
                    m.index = vec!["index.php".to_owned(), "index.html".to_owned()];
                }
                if m.try_files.is_empty() {
                    m.try_files = vec![
                        "$uri".to_owned(),
                        "$uri/".to_owned(),
                        "/index.php?$query_string".to_owned(),
                    ];
                }
            }
            SiteKind::Static => {
                if m.index.is_empty() {
                    m.index = vec!["index.html".to_owned(), "index.htm".to_owned()];
                }
                if m.try_files.is_empty() {
                    m.try_files = vec!["$uri".to_owned(), "$uri/".to_owned(), "=404".to_owned()];
                }
            }
            SiteKind::Proxy | SiteKind::App => {
                m.proxy
                    .as_mut()
                    .ok_or_else(|| invalid("proxy site without proxy settings"))?
                    .normalize()?;
            }
        }
        for i in &m.index {
            check_token("index", i, "")?;
        }
        for t in &m.try_files {
            check_token("try_files entry", t, "")?;
        }
        for d in &m.deny {
            check_token("deny pattern", d, "")?;
        }
        for l in &m.static_locations {
            check_token("static location pattern", &l.pattern, "")?;
            if let Some(cc) = &l.cache_control {
                check_literal("cache_control", cc)?;
            }
        }
        for (k, v) in &m.headers {
            check_token("header name", k, ":()<>@,\\[]?=")?;
            check_literal("header value", v)?;
        }
        for (k, v) in &m.env {
            let ok = k
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(invalid(format!("invalid env name: '{k}'")));
            }
            check_literal("env value", v)?;
        }
        check_token("client_max_body", &m.client_max_body, "")?;
        if let Some(ssl) = &mut m.ssl {
            ssl.cert = posix_string(Path::new(&ssl.cert));
            ssl.key = posix_string(Path::new(&ssl.key));
            check_text("ssl cert", &ssl.cert)?;
            check_text("ssl key", &ssl.key)?;
        }
        m.fallback_resource = m.try_files.last().and_then(|last| {
            let path = last.split('?').next().unwrap_or("");
            (path.starts_with('/') && !path.contains('$')).then(|| path.to_owned())
        });
        Ok(m)
    }

    /// Primary host (first of `hosts`); names the per-site files.
    pub fn primary_host(&self) -> &str {
        self.hosts.first().map(String::as_str).unwrap_or("")
    }
}

/// One backend of a PHP pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamServer {
    /// Address, normally `127.0.0.1`.
    pub host: String,
    /// FastCGI port in the pool range (docs/03 §4).
    pub port: u16,
}

/// A PHP pool seen by the web server (`php_8_3`, docs/06 §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamModel {
    /// Upstream name, `php_<major>_<minor>`.
    pub name: String,
    /// Workers; at least one.
    pub servers: Vec<UpstreamServer>,
    /// Max connections per worker (Apache `max=`); 1 because php-cgi serves one at a time.
    #[serde(default = "default_max_conns")]
    pub max_conns: u32,
    /// Backend timeout in seconds (Apache `timeout=`).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
}

fn default_max_conns() -> u32 {
    1
}
fn default_timeout() -> u32 {
    300
}

impl UpstreamModel {
    /// Pool with default tuning.
    pub fn new(name: impl Into<String>, servers: Vec<UpstreamServer>) -> Self {
        Self {
            name: name.into(),
            servers,
            max_conns: default_max_conns(),
            timeout_secs: default_timeout(),
        }
    }

    /// Validates names and addresses.
    pub fn validate(&self) -> Result<()> {
        check_ident("upstream name", &self.name)?;
        if self.servers.is_empty() {
            return Err(invalid(format!("upstream '{}' has no servers", self.name)));
        }
        for s in &self.servers {
            check_token("upstream host", &s.host, "/")?;
            if s.port == 0 {
                return Err(invalid("upstream port must be positive"));
            }
        }
        Ok(())
    }
}

/// `paths.*` of docs/03 §3.3, all POSIX strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathsModel {
    /// Installation root.
    pub root: String,
    /// `bin/`.
    pub bin: String,
    /// `etc/`.
    pub etc: String,
    /// `data/`.
    pub data: String,
    /// `logs/`.
    pub logs: String,
    /// `tmp/`.
    pub tmp: String,
    /// `www/`.
    pub www: String,
    /// `certs/`.
    pub certs: String,
}

impl PathsModel {
    /// Derives every path from the root.
    pub fn from_root(root: &ForgeRoot) -> Self {
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
        }
    }
}

/// The tool being configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolModel {
    /// Tool id (`nginx`).
    pub id: String,
    /// Exact version.
    pub version: String,
    /// Installation directory of that version (`bin/<tool>/<version>`), POSIX.
    pub dir: String,
}

/// A database/mail instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceModel {
    /// Instance name (`default`).
    pub name: String,
    /// Named ports (`main`; mailpit uses `http` and `smtp`).
    pub ports: BTreeMap<String, u16>,
    /// Data directory, POSIX.
    pub data_dir: String,
    /// Whether `data_dir` carries the NTFS case-sensitive flag (docs/15 section 5). Drives
    /// `lower_case_table_names` in the MySQL/MariaDB templates (0 when true, 2 otherwise).
    #[serde(default = "default_case_sensitive")]
    pub case_sensitive: bool,
    /// Free settings from the UI (quick settings, docs/04).
    pub settings: BTreeMap<String, String>,
}

fn default_case_sensitive() -> bool {
    true
}

impl Default for InstanceModel {
    fn default() -> Self {
        Self {
            name: String::new(),
            ports: BTreeMap::new(),
            data_dir: String::new(),
            case_sensitive: true,
            settings: BTreeMap::new(),
        }
    }
}

/// Global preferences used by templates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefsModel {
    /// Parked directory.
    pub document_root: String,
    /// Default TLD (`test`).
    pub tld: String,
    /// HTTP listen port of the front server.
    pub http_port: u16,
    /// HTTPS listen port of the front server.
    pub https_port: u16,
    /// Port of the daemon's internal HTTP site (`/_forgebox/*`, docs/06 §8).
    pub internal_port: u16,
}

impl PrefsModel {
    /// Defaults for a root: ports 80/443, TLD `test`, internal page on 19000.
    pub fn defaults(root: &ForgeRoot) -> Self {
        Self {
            document_root: posix_string(&root.www()),
            tld: "test".to_owned(),
            http_port: 80,
            https_port: 443,
            internal_port: 19000,
        }
    }
}

/// Xdebug of a PHP series (`conf.d/20-xdebug.ini`), all from the extension's manifest
/// (M2-E2-T9): the `toggle` line and the quick settings, so the template has no Xdebug
/// directive of its own beyond the fixed client host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XdebugModel {
    /// Loaded (`toggle.enable`) or installed but not loaded (`toggle.disable`).
    pub enabled: bool,
    /// The `toggle` line for `enabled` (`zend_extension=php_xdebug.dll`).
    pub line: String,
    /// Effective quick settings as `ini directive = written value`, in manifest order
    /// (`xdebug.mode = debug,develop`, `xdebug.client_port = 9003`).
    pub settings: Vec<IniSetting>,
}

/// One `directive = value` line of a generated ini file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IniSetting {
    pub key: String,
    pub value: String,
}

/// PHP specific context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhpModel {
    /// `8.3`.
    pub series: String,
    /// Enabled extensions (`defaults_on` plus the user's choices).
    pub extensions: Vec<String>,
    /// `zend_extension` entries to load (`opcache` only when it is a separate DLL; PHP 8.5+
    /// builds OPcache in and must leave it out). Filled from the manifest.
    #[serde(default)]
    pub zend_extensions: Vec<String>,
    /// Quick settings overriding the defaults (`memory_limit`...).
    pub settings: BTreeMap<String, String>,
    /// Present when Xdebug is installed.
    pub xdebug: Option<XdebugModel>,
}

/// Everything a template can see (docs/03 §3.3). `site` and `upstream` are added by the
/// renderer when it expands per-site / per-pool files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderContext {
    /// Directories.
    pub paths: PathsModel,
    /// Tool being rendered.
    pub tool: ToolModel,
    /// Same as `tool.version`, kept because docs/03 lists both.
    pub version: String,
    /// Instance (databases, mailpit); empty for web servers.
    pub instance: InstanceModel,
    /// Sites served by this web server.
    pub sites: Vec<VhostModel>,
    /// PHP pools.
    pub pools: Vec<UpstreamModel>,
    /// Preferences.
    pub prefs: PrefsModel,
    /// PHP specifics.
    pub php: Option<PhpModel>,
}

impl RenderContext {
    /// Context for `tool` `version` installed in `root`, with default prefs and no sites.
    pub fn new(root: &ForgeRoot, tool: &str, version: &str) -> Self {
        let dir = root.bin().join(tool).join(version);
        Self {
            paths: PathsModel::from_root(root),
            tool: ToolModel {
                id: tool.to_owned(),
                version: version.to_owned(),
                dir: posix_string(&dir),
            },
            version: version.to_owned(),
            instance: InstanceModel::default(),
            sites: Vec::new(),
            pools: Vec::new(),
            prefs: PrefsModel::defaults(root),
            php: None,
        }
    }

    /// Validates and normalizes sites and pools; php sites must reference a known pool.
    pub fn normalized(&self) -> Result<Self> {
        let mut c = self.clone();
        c.instance.data_dir = posix_string(Path::new(&c.instance.data_dir));
        c.prefs.document_root = posix_string(Path::new(&c.prefs.document_root));
        for pool in &c.pools {
            pool.validate()?;
        }
        c.sites = c
            .sites
            .iter()
            .map(VhostModel::normalized)
            .collect::<Result<Vec<_>>>()?;
        let mut hosts = std::collections::BTreeSet::new();
        for s in &c.sites {
            if !hosts.insert(s.primary_host().to_owned()) {
                return Err(ForgeError::Conflict(format!(
                    "two sites share the primary host '{}'",
                    s.primary_host()
                )));
            }
            if let Some(up) = &s.php_upstream {
                if !c.pools.iter().any(|p| &p.name == up) {
                    return Err(invalid(format!(
                        "site '{}' uses unknown php upstream '{up}'",
                        s.primary_host()
                    )));
                }
            }
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn php_site() -> VhostModel {
        VhostModel {
            hosts: vec!["Loja.TEST".into(), "*.loja.test".into()],
            root: r"C:\www\loja\public".into(),
            php_upstream: Some("php_8_3".into()),
            ..VhostModel::default()
        }
    }

    #[test]
    fn normalizes_hosts_paths_and_defaults() {
        let m = php_site().normalized().unwrap();
        assert_eq!(m.hosts, ["loja.test", "*.loja.test"]);
        assert_eq!(m.root, "C:/www/loja/public");
        assert_eq!(m.index, ["index.php", "index.html"]);
        assert_eq!(m.try_files[2], "/index.php?$query_string");
        assert_eq!(m.fallback_resource.as_deref(), Some("/index.php"));
    }

    #[test]
    fn static_defaults_have_no_fallback() {
        let m = VhostModel {
            kind: SiteKind::Static,
            hosts: vec!["docs.test".into()],
            root: "C:/www/docs".into(),
            ..VhostModel::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(m.try_files.last().unwrap(), "=404");
        assert_eq!(m.fallback_resource, None);
    }

    #[test]
    fn rejects_interpolated_env_and_header_values() {
        for bad in ["base64:ab$cd", "x%{HTTP_HOST}e", "$1"] {
            let mut m = php_site();
            m.env.insert("APP_KEY".into(), bad.into());
            assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
            let mut m = php_site();
            m.headers.insert("X-Test".into(), bad.into());
            assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
        }
    }

    #[test]
    fn rejects_trailing_backslash() {
        let mut m = php_site();
        m.env.insert("APP_DIR".into(), "C:\\data\\".into());
        assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
        let mut m = php_site();
        m.headers.insert("X-Test".into(), "x\\".into());
        assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
        let mut m = php_site();
        m.deny = vec!["/x\\".into()];
        assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
        let mut m = php_site();
        m.index = vec!["index.php\\".into()];
        assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn rejects_backslash_quote_escape_injection() {
        // APP_A ends in a backslash, so it would escape the closing quote and let
        // APP_B / APP_C be parsed as config (reproduced against nginx 1.30.6).
        let payloads = [
            ("APP_A", "x\\"),
            (
                "APP_B",
                "; } location /pwn/ { alias C:/; autoindex on; } location /zz { fastcgi_param Y ",
            ),
            ("APP_C", "; #"),
        ];
        let mut m = php_site();
        for (k, v) in payloads {
            m.env.insert(k.into(), v.into());
        }
        assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn rejects_injection() {
        let mut m = php_site();
        m.hosts = vec!["a.test; return 200".into()];
        assert!(m.normalized().is_err());
        let mut m = php_site();
        m.root = "C:/www/\"x".into();
        assert!(m.normalized().is_err());
        let mut m = php_site();
        m.deny = vec!["/x {".into()];
        assert!(m.normalized().is_err());
        let mut m = php_site();
        m.env.insert("BAD NAME".into(), "v".into());
        assert!(m.normalized().is_err());
        let mut m = php_site();
        m.hosts = vec!["*.loja.test".into()];
        assert!(m.normalized().is_err());
        let mut m = php_site();
        m.php_upstream = None;
        assert!(m.normalized().is_err());
    }

    #[test]
    fn proxy_needs_http_upstream_and_trims_slash() {
        let mut m = VhostModel {
            kind: SiteKind::Proxy,
            hosts: vec!["app.test".into()],
            proxy: Some(ProxyModel {
                websocket: true,
                ..ProxyModel::new("http://127.0.0.1:3000/")
            }),
            ..VhostModel::default()
        };
        let p = m.normalized().unwrap().proxy.unwrap();
        assert_eq!(p.upstream, "http://127.0.0.1:3000");
        assert_eq!(
            (p.scheme.as_str(), p.address.as_str()),
            ("http", "127.0.0.1:3000")
        );
        assert_eq!(p.keepalive_conns, DEFAULT_PROXY_KEEPALIVE_CONNS);
        m.proxy.as_mut().unwrap().upstream = "ftp://x".into();
        assert!(m.normalized().is_err());
    }

    #[test]
    fn proxy_upstream_is_split_for_the_templates() {
        let split = |upstream: &str| {
            let m = VhostModel {
                kind: SiteKind::Proxy,
                hosts: vec!["app.test".into()],
                proxy: Some(ProxyModel::new(upstream)),
                ..VhostModel::default()
            };
            m.normalized()
                .map(|m| {
                    let p = m.proxy.unwrap();
                    (p.scheme, p.address, p.host)
                })
                .map_err(|e| e.code())
        };
        let ok = |s: &str, a: &str, h: &str| Ok((s.to_owned(), a.to_owned(), h.to_owned()));
        assert_eq!(
            split("http://127.0.0.1:3000"),
            ok("http", "127.0.0.1:3000", "127.0.0.1")
        );
        assert_eq!(
            split("http://localhost"),
            ok("http", "localhost:80", "localhost")
        );
        assert_eq!(
            split("https://backend.internal/"),
            ok("https", "backend.internal:443", "backend.internal")
        );
        assert_eq!(split("http://[::1]:5173"), ok("http", "[::1]:5173", "::1"));
        assert_eq!(split("https://[::1]"), ok("https", "[::1]:443", "::1"));
        for bad in [
            "http://x:1/api",
            "http://u@x:1",
            "http://x:1?a",
            "http://x:0",
            "http://x:99999",
            "http://x:",
            "http://:80",
            "http://[::1",
            "http://[zz]:1",
            "http://[::1]x",
            "http://*.x:1",
            "http://a:b:c",
        ] {
            assert_eq!(split(bad), Err("invalid_input"), "{bad}");
        }
    }

    #[test]
    fn proxy_keepalive_needs_an_idle_time() {
        let mut m = VhostModel {
            kind: SiteKind::Proxy,
            hosts: vec!["app.test".into()],
            proxy: Some(ProxyModel {
                keepalive_idle_secs: 0,
                ..ProxyModel::new("http://127.0.0.1:3000")
            }),
            ..VhostModel::default()
        };
        assert_eq!(m.normalized().unwrap_err().code(), "invalid_input");
        m.proxy.as_mut().unwrap().keepalive_conns = 0;
        assert!(m.normalized().is_ok());
    }

    #[test]
    fn context_checks_pools_and_duplicate_hosts() {
        let root = ForgeRoot::new(std::env::temp_dir().join("fb-model")).unwrap();
        let mut ctx = RenderContext::new(&root, "nginx", "1.30.6");
        ctx.sites.push(php_site());
        assert_eq!(ctx.normalized().unwrap_err().code(), "invalid_input");
        ctx.pools.push(UpstreamModel::new(
            "php_8_3",
            vec![UpstreamServer {
                host: "127.0.0.1".into(),
                port: 19083,
            }],
        ));
        assert!(ctx.normalized().is_ok());
        ctx.sites.push(php_site());
        assert_eq!(ctx.normalized().unwrap_err().code(), "conflict");
    }

    #[test]
    fn upstream_requires_servers() {
        assert!(UpstreamModel::new("php_8_3", vec![]).validate().is_err());
    }
}
