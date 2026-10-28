//! `Site` to `VhostModel` (docs/06 §1.2): the common part of every driver.
//!
//! [`base_model`] fills what does not depend on the framework: hosts (primary, its
//! wildcard, aliases), document root, PHP pool upstream, denied paths, TLS files of
//! `certs/sites/<host>` and the user's per-server `extra`. Drivers refine it in
//! `Driver::tune`; [`build`] runs the whole pipeline and validates the result.

use forge_core::error::{ForgeError, Result};
use forge_core::paths::posix_string;
use forge_core::store::sites::{Site, SiteKind as StoredKind, SitePhp};
use forge_manifest::schema::project::{ProcessAutostart, ProcessPort};
use forge_render::{SiteKind, SslModel, VhostModel};

use crate::drivers::{self, Driver, VhostContext};
use crate::{apps, proxy};

/// Paths answered with 403 on every site: `.htaccess`/`.htpasswd`, `.env*`, `.git*`.
pub fn default_deny() -> Vec<String> {
    [r"/\.ht", r"/\.env", r"/\.git"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Name of the PHP pool upstream for a site: `php_<series with _>` (`php_8_3`), or
/// `php_<version with _>` (`php_8_3_36`) when the site pins an exact version, because an
/// exact version has its own pool (docs/06 §2.1).
pub fn php_upstream_name(php: &SitePhp, default_series: &str) -> String {
    let version = php
        .exact
        .as_deref()
        .or(php.series.as_deref())
        .unwrap_or(default_series);
    format!("php_{}", version.replace('.', "_"))
}

fn render_kind(kind: StoredKind) -> SiteKind {
    match kind {
        StoredKind::Php => SiteKind::Php,
        StoredKind::Static => SiteKind::Static,
        StoredKind::Proxy => SiteKind::Proxy,
        StoredKind::App => SiteKind::App,
    }
}

/// The framework-independent model of `site`. `kind` is the kind of the driver (so a
/// site whose driver was overridden renders as that driver says).
pub fn base_model(site: &Site, ctx: &VhostContext, kind: StoredKind) -> VhostModel {
    let mut hosts = vec![site.host.clone(), format!("*.{}", site.host)];
    hosts.extend(site.aliases.iter().cloned());
    let secure = site.secure;
    let ssl = secure.then(|| SslModel {
        cert: posix_string(&ctx.certs_sites.join(format!("{}.crt", site.host))),
        key: posix_string(&ctx.certs_sites.join(format!("{}.key", site.host))),
    });
    VhostModel {
        kind: render_kind(kind),
        hosts,
        root: site.root.clone().unwrap_or_default(),
        php_upstream: (kind == StoredKind::Php)
            .then(|| php_upstream_name(&site.php, &ctx.default_php_series)),
        deny: default_deny(),
        ssl,
        http_redirect: secure && site.http_redirect,
        env: site.env.clone(),
        extra: ctx.extra.clone(),
        ..VhostModel::default()
    }
}

/// `kind: proxy`: the upstream the user gave, with the options of the host (or the
/// defaults: WebSocket on).
fn proxy_model(site: &Site, ctx: &VhostContext) -> Result<VhostModel> {
    let upstream = site.upstream.clone().ok_or_else(|| {
        ForgeError::InvalidInput(format!("proxy site {} without upstream", site.host))
    })?;
    let options = ctx
        .proxy_options
        .get(&site.host)
        .cloned()
        .unwrap_or_default();
    proxy::proxy_vhost(site, ctx, &upstream, &options)
}

/// `kind: app`: the host proxies to the process that declares `proxy: true`, listening on
/// `127.0.0.1:$PORT` (docs/06 §4.2).
fn app_model(site: &Site, ctx: &VhostContext) -> Result<VhostModel> {
    let processes = apps::processes_of(&site.processes)?;
    let (name, spec) = apps::proxied_process(&site.host, &processes)?;
    if spec.proxy_path.as_deref().is_some_and(|p| p != "/") {
        return Err(ForgeError::NotImplemented(format!(
            "{}: proxy_path on process {name} (only whole-host proxies are rendered)",
            site.host
        )));
    }
    let port = match spec.port {
        Some(ProcessPort::Fixed(port)) => port,
        _ => ctx.app_ports.get(name).copied().ok_or_else(|| {
            ForgeError::Conflict(format!(
                "{}: the port of process {name} is not allocated yet",
                site.host
            ))
        })?,
    };
    let mut options = ctx
        .proxy_options
        .get(&site.host)
        .cloned()
        .unwrap_or_default();
    options.start_on_visit = spec.autostart == ProcessAutostart::OnVisit;
    proxy::proxy_vhost(site, ctx, &proxy::loopback_upstream(port), &options)
}

/// Complete, validated vhost of `site`: picks the driver (`site.driver`; `generic-php`
/// or `static` when unset), asks it for the model and runs `VhostModel::normalized`.
pub fn build(site: &Site, ctx: &VhostContext) -> Result<VhostModel> {
    let model = match site.kind {
        StoredKind::Proxy => proxy_model(site, ctx)?,
        StoredKind::App => app_model(site, ctx)?,
        StoredKind::Php | StoredKind::Static => {
            let id = site.driver.as_deref().unwrap_or(match site.kind {
                StoredKind::Static => "static",
                _ => "generic-php",
            });
            let driver: &dyn Driver = drivers::by_id(id)
                .ok_or_else(|| ForgeError::ConfigInvalid(format!("unknown driver {id:?}")))?;
            driver.vhost(site, ctx)
        }
    };
    model.normalized()
}
