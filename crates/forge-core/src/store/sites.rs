//! `sites` table: the persisted [`Site`] (docs/06 §1) and its repository.
//!
//! The repository enforces what SQL cannot: hosts and aliases are unique **globally**
//! (a name used as the host of one site cannot be an alias of another). Host syntax
//! (TLD, characters) is validated by `forge-sites`; here only lowercase is checked, as
//! the table does (`CHECK (host = lower(host))`, docs/15 §7).

use std::collections::BTreeMap;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, Result};
use crate::ids::SiteId;

/// Kind of site, as stored in `sites.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteKind {
    /// PHP through a FastCGI pool.
    #[default]
    Php,
    /// Plain files.
    Static,
    /// Reverse proxy to an upstream URL.
    Proxy,
    /// Proxy to a Procfile process of the site.
    App,
}

impl SiteKind {
    /// Stored name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Php => "php",
            Self::Static => "static",
            Self::Proxy => "proxy",
            Self::App => "app",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "php" => Self::Php,
            "static" => Self::Static,
            "proxy" => Self::Proxy,
            "app" => Self::App,
            other => return Err(ForgeError::Schema(format!("sites.kind {other:?}"))),
        })
    }
}

/// Xdebug mode of a site (`sites.xdebug`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteXdebug {
    /// Follow the PHP tool settings.
    #[default]
    Inherit,
    /// Disabled for this site.
    Off,
    /// Only with the trigger cookie/param.
    Trigger,
    /// Every request.
    Always,
}

impl SiteXdebug {
    /// Stored name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Off => "off",
            Self::Trigger => "trigger",
            Self::Always => "always",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "inherit" => Self::Inherit,
            "off" => Self::Off,
            "trigger" => Self::Trigger,
            "always" => Self::Always,
            other => return Err(ForgeError::Schema(format!("sites.xdebug {other:?}"))),
        })
    }
}

/// PHP selection of a `php` site: a series follows the latest patch, an exact version is
/// pinned (docs/06 §2.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SitePhp {
    /// `"8.3"`.
    pub series: Option<String>,
    /// `"8.3.36"`; wins over `series` when both are set.
    pub exact: Option<String>,
    /// Per-site pool overrides (JSON object).
    pub pool_overrides: Option<serde_json::Value>,
}

/// A site (vhost, proxy or app), docs/06 §1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Site {
    pub id: SiteId,
    /// Derived from the host (`loja-test`).
    pub slug: String,
    /// Lowercase primary host.
    pub host: String,
    pub aliases: Vec<String>,
    pub kind: SiteKind,
    /// Document root (php/static), absolute.
    pub root: Option<String>,
    /// Driver id (`laravel`...), `None` for proxies.
    pub driver: Option<String>,
    /// Front web server tool id; `None` = `prefs.default_server`.
    pub server: Option<String>,
    pub php: SitePhp,
    /// Upstream URL (proxy sites).
    pub upstream: Option<String>,
    pub secure: bool,
    pub http_redirect: bool,
    pub env: BTreeMap<String, String>,
    pub xdebug: SiteXdebug,
    /// Created implicitly from a parked directory.
    pub parked: bool,
    pub enabled: bool,
    pub case_sensitive: bool,
    /// Procfile processes (shape owned by `forge-sites::apps`, M1-E4-T5).
    pub processes: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Site {
    /// A minimal enabled PHP site with `secure`/`http_redirect` on, for `host`/`slug`.
    pub fn new(host: impl Into<String>, slug: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: SiteId::new(),
            slug: slug.into(),
            host: host.into(),
            aliases: Vec::new(),
            kind: SiteKind::Php,
            root: None,
            driver: None,
            server: None,
            php: SitePhp::default(),
            upstream: None,
            secure: true,
            http_redirect: true,
            env: BTreeMap::new(),
            xdebug: SiteXdebug::Inherit,
            parked: false,
            enabled: true,
            case_sensitive: true,
            processes: serde_json::Value::Array(Vec::new()),
            created_at: now,
            updated_at: now,
        }
    }

    /// Every name of the site: host first, then aliases.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.host.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

const COLUMNS: &str = "id, slug, host, aliases, kind, root, driver, server, php_series, \
    php_exact, php_pool_overrides, upstream, secure, http_redirect, env, xdebug, parked, \
    enabled, case_sensitive, processes, created_at, updated_at";

fn corrupt(what: &str, e: impl ToString) -> ForgeError {
    ForgeError::Schema(format!("sites row: invalid {what}: {}", e.to_string()))
}

/// Reads one row; the inner `Result` carries decoding problems of JSON/enum columns.
fn from_row(row: &Row<'_>) -> rusqlite::Result<Result<Site>> {
    let id: SiteId = row.get(0)?;
    let slug: String = row.get(1)?;
    let host: String = row.get(2)?;
    let aliases: String = row.get(3)?;
    let kind: String = row.get(4)?;
    let root = row.get(5)?;
    let driver = row.get(6)?;
    let server = row.get(7)?;
    let series = row.get(8)?;
    let exact = row.get(9)?;
    let overrides: Option<String> = row.get(10)?;
    let upstream = row.get(11)?;
    let secure: bool = row.get(12)?;
    let http_redirect: bool = row.get(13)?;
    let env: String = row.get(14)?;
    let xdebug: String = row.get(15)?;
    let parked: bool = row.get(16)?;
    let enabled: bool = row.get(17)?;
    let case_sensitive: bool = row.get(18)?;
    let processes: String = row.get(19)?;
    let created: String = row.get(20)?;
    let updated: String = row.get(21)?;
    let decode = || -> Result<Site> {
        Ok(Site {
            id,
            slug,
            host,
            aliases: serde_json::from_str(&aliases).map_err(|e| corrupt("aliases", e))?,
            kind: SiteKind::parse(&kind)?,
            root,
            driver,
            server,
            php: SitePhp {
                series,
                exact,
                pool_overrides: overrides
                    .map(|o| serde_json::from_str(&o).map_err(|e| corrupt("pool overrides", e)))
                    .transpose()?,
            },
            upstream,
            secure,
            http_redirect,
            env: serde_json::from_str(&env).map_err(|e| corrupt("env", e))?,
            xdebug: SiteXdebug::parse(&xdebug)?,
            parked,
            enabled,
            case_sensitive,
            processes: serde_json::from_str(&processes).map_err(|e| corrupt("processes", e))?,
            created_at: created.parse().map_err(|e| corrupt("created_at", e))?,
            updated_at: updated.parse().map_err(|e| corrupt("updated_at", e))?,
        })
    };
    Ok(decode())
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| ForgeError::Internal(format!("site json: {e}")))
}

/// Repository over the `sites` table.
#[derive(Debug, Clone, Copy)]
pub struct SiteRepo<'c> {
    conn: &'c Connection,
}

impl<'c> SiteRepo<'c> {
    /// Repository on `conn` (writer operation, reader or maintenance connection).
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    fn check(site: &Site) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for name in site.names() {
            if name != name.to_ascii_lowercase() {
                return Err(ForgeError::InvalidInput(format!(
                    "host must be lowercase: {name:?}"
                )));
            }
            if !seen.insert(name) {
                return Err(ForgeError::InvalidInput(format!(
                    "duplicate name in site: {name:?}"
                )));
            }
        }
        Ok(())
    }

    /// Site that already uses `name` as host or alias, ignoring `except`.
    pub fn owner_of_name(&self, name: &str, except: Option<SiteId>) -> Result<Option<Site>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM sites WHERE (host = ?1 OR EXISTS \
             (SELECT 1 FROM json_each(sites.aliases) WHERE value = ?1)) \
             AND (?2 IS NULL OR id != ?2)"
        );
        self.conn
            .query_row(&sql, params![name, except], from_row)
            .optional()?
            .transpose()
    }

    fn check_names_free(&self, site: &Site) -> Result<()> {
        for name in site.names() {
            if let Some(owner) = self.owner_of_name(name, Some(site.id))? {
                return Err(ForgeError::Conflict(format!(
                    "{name} is already used by site {}",
                    owner.host
                )));
            }
        }
        let slug_owner = self
            .conn
            .query_row(
                "SELECT host FROM sites WHERE slug = ?1 AND id != ?2",
                params![site.slug, site.id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(other) = slug_owner {
            return Err(ForgeError::Conflict(format!(
                "slug {} is already used by site {other}",
                site.slug
            )));
        }
        Ok(())
    }

    /// Inserts `site`. `Conflict` if the host, an alias or the slug is taken.
    pub fn insert(&self, site: &Site) -> Result<()> {
        Self::check(site)?;
        self.check_names_free(site)?;
        let overrides = site.php.pool_overrides.as_ref().map(json).transpose()?;
        self.conn.execute(
            &format!(
                "INSERT INTO sites ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)"
            ),
            params![
                site.id,
                site.slug,
                site.host,
                json(&site.aliases)?,
                site.kind.as_str(),
                site.root,
                site.driver,
                site.server,
                site.php.series,
                site.php.exact,
                overrides,
                site.upstream,
                site.secure,
                site.http_redirect,
                json(&site.env)?,
                site.xdebug.as_str(),
                site.parked,
                site.enabled,
                site.case_sensitive,
                json(&site.processes)?,
                site.created_at.to_string(),
                site.updated_at.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Replaces every column of the site with `site.id`. `NotFound` if it does not exist;
    /// `Conflict` as in [`SiteRepo::insert`].
    pub fn update(&self, site: &Site) -> Result<()> {
        Self::check(site)?;
        self.check_names_free(site)?;
        let overrides = site.php.pool_overrides.as_ref().map(json).transpose()?;
        let n = self.conn.execute(
            "UPDATE sites SET slug = ?2, host = ?3, aliases = ?4, kind = ?5, root = ?6, \
             driver = ?7, server = ?8, php_series = ?9, php_exact = ?10, \
             php_pool_overrides = ?11, upstream = ?12, secure = ?13, http_redirect = ?14, \
             env = ?15, xdebug = ?16, parked = ?17, enabled = ?18, case_sensitive = ?19, \
             processes = ?20, updated_at = ?21 WHERE id = ?1",
            params![
                site.id,
                site.slug,
                site.host,
                json(&site.aliases)?,
                site.kind.as_str(),
                site.root,
                site.driver,
                site.server,
                site.php.series,
                site.php.exact,
                overrides,
                site.upstream,
                site.secure,
                site.http_redirect,
                json(&site.env)?,
                site.xdebug.as_str(),
                site.parked,
                site.enabled,
                site.case_sensitive,
                json(&site.processes)?,
                site.updated_at.to_string(),
            ],
        )?;
        if n == 0 {
            return Err(ForgeError::NotFound(format!("site {}", site.id)));
        }
        Ok(())
    }

    /// Site by id.
    pub fn get(&self, id: SiteId) -> Result<Option<Site>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM sites WHERE id = ?1"),
                [id],
                from_row,
            )
            .optional()?
            .transpose()
    }

    /// Site whose primary host is `host`.
    pub fn get_by_host(&self, host: &str) -> Result<Option<Site>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM sites WHERE host = ?1"),
                [host],
                from_row,
            )
            .optional()?
            .transpose()
    }

    /// Every site ordered by host.
    pub fn list(&self) -> Result<Vec<Site>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM sites ORDER BY host"))?;
        let rows = stmt.query_map([], from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    /// Deletes the site; returns whether it existed.
    pub fn delete(&self, id: SiteId) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM sites WHERE id = ?1", [id])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn site(host: &str) -> Site {
        let mut s = Site::new(host, host.replace('.', "-"));
        s.root = Some("C:/www/x/public".to_owned());
        s.driver = Some("laravel".to_owned());
        s.env.insert("APP_ENV".to_owned(), "local".to_owned());
        s
    }

    #[test]
    fn roundtrip_and_list() {
        let store = Store::open_in_memory().unwrap();
        let repo = SiteRepo::new(store.conn());
        let mut a = site("a.test");
        a.aliases = vec!["admin.a.test".to_owned()];
        a.php.series = Some("8.3".to_owned());
        a.php.pool_overrides = Some(serde_json::json!({"max_children": 4}));
        repo.insert(&a).unwrap();
        repo.insert(&site("b.test")).unwrap();
        assert_eq!(repo.get(a.id).unwrap().unwrap(), a);
        assert_eq!(repo.get_by_host("b.test").unwrap().unwrap().host, "b.test");
        let hosts: Vec<_> = repo.list().unwrap().into_iter().map(|s| s.host).collect();
        assert_eq!(hosts, ["a.test", "b.test"]);
        assert!(repo.delete(a.id).unwrap());
        assert!(!repo.delete(a.id).unwrap());
        assert!(repo.get(a.id).unwrap().is_none());
    }

    #[test]
    fn names_are_unique_across_hosts_and_aliases() {
        let store = Store::open_in_memory().unwrap();
        let repo = SiteRepo::new(store.conn());
        let mut a = site("a.test");
        a.aliases = vec!["shared.test".to_owned()];
        repo.insert(&a).unwrap();
        let mut b = site("b.test");
        b.aliases = vec!["shared.test".to_owned()];
        assert!(matches!(repo.insert(&b), Err(ForgeError::Conflict(_))));
        assert!(matches!(
            repo.insert(&site("shared.test")),
            Err(ForgeError::Conflict(_))
        ));
        let mut d = site("d.test");
        d.aliases = vec!["a.test".to_owned()];
        assert!(matches!(repo.insert(&d), Err(ForgeError::Conflict(_))));
        // updating a site with its own names is fine
        let mut a2 = a.clone();
        a2.secure = false;
        repo.update(&a2).unwrap();
    }

    #[test]
    fn uppercase_and_missing_are_rejected() {
        let store = Store::open_in_memory().unwrap();
        let repo = SiteRepo::new(store.conn());
        assert!(matches!(
            repo.insert(&site("Loja.test")),
            Err(ForgeError::InvalidInput(_))
        ));
        assert!(matches!(
            repo.update(&site("x.test")),
            Err(ForgeError::NotFound(_))
        ));
    }
}
