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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyModel {
    /// `http://127.0.0.1:3000` (no trailing slash).
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
}

fn default_read_timeout() -> u32 {
    3600
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
                let p = m
                    .proxy
                    .as_mut()
                    .ok_or_else(|| invalid("proxy site without proxy settings"))?;
                p.upstream = p.upstream.trim_end_matches('/').to_owned();
                check_token("proxy upstream", &p.upstream, "")?;
                if !(p.upstream.starts_with("http://") || p.upstream.starts_with("https://")) {
                    return Err(invalid(format!(
                        "proxy upstream must be http:// or https://: '{}'",
                        p.upstream
                    )));
                }
                if p.read_timeout_secs == 0 {
                    return Err(invalid("proxy read timeout must be positive"));
                }
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceModel {
    /// Instance name (`default`).
    pub name: String,
    /// Named ports (`main`; mailpit uses `http` and `smtp`).
    pub ports: BTreeMap<String, u16>,
    /// Data directory, POSIX.
    pub data_dir: String,
    /// Free settings from the UI (quick settings, docs/04).
    pub settings: BTreeMap<String, String>,
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

/// Xdebug settings of a PHP series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XdebugModel {
    /// `off`, `debug`, `develop`, `coverage`...
    pub mode: String,
    /// `trigger`, `yes`, `no`.
    pub start_with_request: String,
    /// Client port (9003).
    pub client_port: u16,
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
                upstream: "http://127.0.0.1:3000/".into(),
                websocket: true,
                read_timeout_secs: 3600,
                start_on_visit: false,
            }),
            ..VhostModel::default()
        };
        assert_eq!(
            m.normalized().unwrap().proxy.unwrap().upstream,
            "http://127.0.0.1:3000"
        );
        m.proxy.as_mut().unwrap().upstream = "ftp://x".into();
        assert!(m.normalized().is_err());
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
