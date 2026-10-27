//! The site model (docs/06 §1) and the validation of its names.
//!
//! The persisted [`Site`] and its repository live in `forge-core` (`store::sites`) so the
//! store does not depend on this crate; this module re-exports them and adds what is
//! specific to sites: host/alias normalization ([`normalize_host`]), slugs
//! ([`slug_for_host`]) and [`SiteDraft`], the input of `site.create`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::paths::posix_string;
pub use forge_core::store::sites::{Site, SiteKind, SitePhp, SiteRepo, SiteXdebug};
use forge_manifest::schema::project::ProcessSpec;
use jiff::Timestamp;

use crate::apps;
use crate::drivers;
use crate::messages;

/// A non-fatal remark about the user's input, with its i18n key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// i18n key (`sites.host_lowercased`).
    pub key: &'static str,
    /// pt-BR text.
    pub message: String,
}

impl From<(&'static str, String)> for Warning {
    fn from((key, message): (&'static str, String)) -> Self {
        Self { key, message }
    }
}

/// A validated name and what was changed to get there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    pub value: String,
    pub warnings: Vec<Warning>,
}

/// Rules that depend on preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteRules {
    /// Accepted TLDs of primary hosts, without the dot (`test`).
    pub tlds: Vec<String>,
}

impl Default for SiteRules {
    fn default() -> Self {
        Self {
            tlds: vec!["test".to_owned()],
        }
    }
}

impl SiteRules {
    /// Rules accepting `tlds` (given with or without a leading dot, any case).
    pub fn new<I, S>(tlds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            tlds: tlds
                .into_iter()
                .map(|t| t.as_ref().trim_start_matches('.').to_ascii_lowercase())
                .collect(),
        }
    }
}

fn invalid(host: &str, why: &str) -> ForgeError {
    ForgeError::InvalidInput(messages::host_invalid(host, why).1)
}

/// Syntax of a DNS name: labels of `[a-z0-9-]`, 1..=63 chars, no leading/trailing `-`,
/// at most 253 chars in total, at least two labels. `host` must already be lowercase.
fn check_syntax(raw: &str, host: &str) -> Result<()> {
    if host.is_empty() || host.len() > 253 {
        return Err(invalid(raw, "tamanho inválido"));
    }
    let mut labels = 0;
    for label in host.split('.') {
        labels += 1;
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok {
            return Err(invalid(
                raw,
                "use apenas letras minúsculas, números, '.' e '-'",
            ));
        }
    }
    if labels < 2 {
        return Err(invalid(raw, "faltou o TLD"));
    }
    Ok(())
}

fn normalize_case(raw: &str) -> (String, Vec<Warning>) {
    let host = raw.trim().to_ascii_lowercase();
    let mut warnings = Vec::new();
    if host != raw.trim() {
        warnings.push(messages::host_lowercased(raw.trim(), &host).into());
    }
    (host, warnings)
}

/// Primary host: lowercased (with a warning), `[a-z0-9.-]`, ending in a configured TLD
/// (docs/15 §7). Uppercase is normalized, never rejected.
pub fn normalize_host(raw: &str, rules: &SiteRules) -> Result<Normalized> {
    let (value, warnings) = normalize_case(raw);
    check_syntax(raw, &value)?;
    let tld = value.rsplit('.').next().unwrap_or("");
    if !rules.tlds.iter().any(|t| t == tld) {
        return Err(ForgeError::InvalidInput(
            messages::host_tld(&value, &rules.tlds).1,
        ));
    }
    Ok(Normalized { value, warnings })
}

/// Alias: same as [`normalize_host`] without the TLD restriction (`loja.local` is fine).
pub fn normalize_alias(raw: &str) -> Result<Normalized> {
    let (value, warnings) = normalize_case(raw);
    check_syntax(raw, &value)?;
    Ok(Normalized { value, warnings })
}

/// Slug derived from the host: dots become dashes (`loja.test` is `loja-test`).
pub fn slug_for_host(host: &str) -> String {
    host.to_ascii_lowercase().replace('.', "-")
}

/// Host label for a folder name: lowercase, anything outside `[a-z0-9-]` (dots included)
/// becomes `-`, edges trimmed. `None` when nothing usable is left or it exceeds 63 chars.
pub fn host_label_for_folder(name: &str) -> Option<String> {
    let label: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let label = label.trim_matches('-').to_owned();
    (!label.is_empty() && label.len() <= 63).then_some(label)
}

/// Input of `site.create`: everything the user (or a parked scan, or a `forgebox.yaml`)
/// can decide; ids, slug and timestamps are produced by [`SiteDraft::into_site`].
#[derive(Debug, Clone, PartialEq)]
pub struct SiteDraft {
    pub host: String,
    pub aliases: Vec<String>,
    pub kind: SiteKind,
    /// Document root, absolute.
    pub root: Option<PathBuf>,
    pub driver: Option<String>,
    pub server: Option<String>,
    pub php: SitePhp,
    pub upstream: Option<String>,
    pub secure: bool,
    pub http_redirect: bool,
    pub env: BTreeMap<String, String>,
    pub xdebug: SiteXdebug,
    pub parked: bool,
    /// Procfile processes (`forgebox.yaml spec.processes`, docs/06 §4.2). A `kind: app`
    /// site needs exactly one with `proxy: true`; its `root` is then the project folder.
    pub processes: BTreeMap<String, ProcessSpec>,
}

impl SiteDraft {
    /// Draft for a project folder: runs driver detection (or uses `driver_override`) and
    /// takes root and kind from the driver. TLS on, PHP selection left to the caller.
    ///
    /// **Blocking**: detection does many `stat`/`read_dir` calls. Async callers (the
    /// daemon) must run it through `spawn_blocking` (ADR-0011), like `ProjectPlan::load`.
    /// Callers that already have a detection result use [`SiteDraft::from_detected`].
    pub fn from_dir(dir: &Path, host: &str, driver_override: Option<&str>) -> Result<Self> {
        let found = drivers::detect(dir, driver_override)?;
        Ok(Self::from_detected(host, found.driver, found.root))
    }

    /// Draft from an already detected driver and document root. Does no I/O.
    pub fn from_detected(host: &str, driver: &dyn drivers::Driver, root: PathBuf) -> Self {
        Self {
            host: host.to_owned(),
            aliases: Vec::new(),
            kind: driver.kind(),
            root: Some(root),
            driver: Some(driver.id().to_owned()),
            server: None,
            php: SitePhp::default(),
            upstream: None,
            secure: true,
            http_redirect: true,
            env: BTreeMap::new(),
            xdebug: SiteXdebug::Inherit,
            parked: false,
            processes: BTreeMap::new(),
        }
    }

    /// Validates the draft and builds the [`Site`] (new id, slug, timestamps).
    /// Returns the normalization warnings (uppercase host...).
    pub fn into_site(self, rules: &SiteRules) -> Result<(Site, Vec<Warning>)> {
        let host = normalize_host(&self.host, rules)?;
        let mut warnings = host.warnings;
        let mut aliases: Vec<String> = Vec::new();
        for raw in &self.aliases {
            let alias = normalize_alias(raw)?;
            warnings.extend(alias.warnings);
            if alias.value == host.value {
                return Err(ForgeError::InvalidInput(format!(
                    "o alias '{raw}' é igual ao host do site"
                )));
            }
            if !aliases.contains(&alias.value) {
                aliases.push(alias.value);
            }
        }
        match self.kind {
            SiteKind::Php | SiteKind::Static if self.root.is_none() => {
                return Err(ForgeError::InvalidInput(
                    "um site php/static precisa de root".to_owned(),
                ));
            }
            SiteKind::Proxy if self.upstream.is_none() => {
                return Err(ForgeError::InvalidInput(
                    "um site proxy precisa de upstream".to_owned(),
                ));
            }
            SiteKind::App if self.root.is_none() => {
                return Err(ForgeError::InvalidInput(
                    "um site app precisa de root (a pasta do projeto)".to_owned(),
                ));
            }
            _ => {}
        }
        apps::validate_processes(&host.value, &self.processes)?;
        if self.kind == SiteKind::App {
            // The upstream of an app is its process' port: one process takes the host.
            apps::proxied_process(&host.value, &self.processes)?;
        }
        if let Some(id) = &self.driver {
            if drivers::by_id(id).is_none() {
                return Err(ForgeError::InvalidInput(format!(
                    "driver desconhecido: {id}"
                )));
            }
        }
        let now = Timestamp::now();
        let mut site = Site::new(host.value.clone(), slug_for_host(&host.value));
        site.aliases = aliases;
        site.kind = self.kind;
        site.root = self.root.as_deref().map(posix_string);
        site.driver = self.driver;
        site.server = self.server;
        site.php = self.php;
        site.upstream = self.upstream;
        site.secure = self.secure;
        site.http_redirect = self.http_redirect && self.secure;
        site.env = self.env;
        site.xdebug = self.xdebug;
        site.parked = self.parked;
        site.processes = apps::processes_to_json(&self.processes);
        site.created_at = now;
        site.updated_at = now;
        Ok((site, warnings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_lowercased_with_a_warning() {
        let n = normalize_host("Loja.TEST", &SiteRules::default()).unwrap();
        assert_eq!(n.value, "loja.test");
        assert_eq!(n.warnings.len(), 1);
        assert_eq!(n.warnings[0].key, "sites.host_lowercased");
        assert!(
            normalize_host("loja.test", &SiteRules::default())
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn invalid_hosts_are_rejected() {
        let rules = SiteRules::default();
        for bad in [
            "loja_x.test",
            "loja test",
            "-a.test",
            "a-.test",
            "a..test",
            ".test",
            "test",
            "loja.com",
            "lója.test",
            "*.loja.test",
            "",
            "a/b.test",
        ] {
            assert!(normalize_host(bad, &rules).is_err(), "{bad:?}");
        }
        assert!(normalize_host(&format!("{}.test", "a".repeat(64)), &rules).is_err());
        assert!(normalize_host("a.b.test", &rules).is_ok());
        assert!(normalize_host("x1-y2.local", &SiteRules::new([".LOCAL", "test"])).is_ok());
    }

    #[test]
    fn aliases_do_not_need_the_tld_but_need_valid_syntax() {
        assert_eq!(normalize_alias("Loja.local").unwrap().value, "loja.local");
        assert!(normalize_alias("bad host.local").is_err());
        assert!(normalize_alias("*.x.test").is_err());
    }

    #[test]
    fn slugs_and_folder_labels() {
        assert_eq!(slug_for_host("admin.loja.test"), "admin-loja-test");
        assert_eq!(host_label_for_folder("Loja").as_deref(), Some("loja"));
        assert_eq!(
            host_label_for_folder("Minha Loja_2").as_deref(),
            Some("minha-loja-2")
        );
        assert_eq!(host_label_for_folder("foo.bar").as_deref(), Some("foo-bar"));
        assert_eq!(host_label_for_folder("---"), None);
        assert_eq!(host_label_for_folder(&"x".repeat(64)), None);
    }

    #[test]
    fn draft_builds_a_site() {
        let draft = SiteDraft {
            host: "Loja.test".to_owned(),
            aliases: vec!["Admin.Loja.test".to_owned(), "admin.loja.test".to_owned()],
            kind: SiteKind::Php,
            root: Some(PathBuf::from("C:/www/loja/public")),
            driver: Some("laravel".to_owned()),
            server: None,
            php: SitePhp::default(),
            upstream: None,
            secure: false,
            http_redirect: true,
            env: BTreeMap::new(),
            xdebug: SiteXdebug::Inherit,
            parked: false,
            processes: BTreeMap::new(),
        };
        let (site, warnings) = draft.clone().into_site(&SiteRules::default()).unwrap();
        assert_eq!(site.host, "loja.test");
        assert_eq!(site.slug, "loja-test");
        assert_eq!(site.aliases, ["admin.loja.test"]);
        assert!(!site.http_redirect, "no redirect without TLS");
        assert_eq!(warnings.len(), 2);
        let mut bad = draft;
        bad.driver = Some("nope".to_owned());
        assert!(bad.into_site(&SiteRules::default()).is_err());
    }
}
