//! Reverse-proxy sites (docs/06 §4.1): `host → upstream` with WebSocket, extra response
//! headers and TLS, turned into the server-neutral [`VhostModel`] that `forge-render`
//! translates for nginx, Apache and Caddy.
//!
//! Two kinds of site end up here:
//! * `kind: proxy` — the user gives the upstream (`forge proxy add app.test
//!   http://127.0.0.1:3000 --ws --secure`);
//! * `kind: app` — the upstream is `http://127.0.0.1:$PORT` of the site's process that
//!   declares `proxy: true` ([`crate::apps`]); the port comes from the apps range
//!   ([`VhostContext::app_ports`]).
//!
//! WebSocket upgrades are forwarded by default: HMR (Next.js, Vite) needs them and a
//! proxy that silently drops them is the most common "my dev server is broken" report.
//! [`ProxyOptions`] switches it off (`websocket: false` in `forgebox.yaml`).

use std::collections::BTreeMap;

use forge_core::error::{ForgeError, Result};
use forge_core::store::sites::Site;
use forge_render::{ProxyModel, SiteKind, VhostModel};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::drivers::VhostContext;
use crate::messages;
use crate::vhost::base_model;

/// Read timeout of a proxied request (docs/06 §4.1: `proxy_read_timeout 1h`, so an idle
/// HMR socket is not cut).
pub const DEFAULT_READ_TIMEOUT_SECS: u32 = 3600;

/// How a site is proxied; everything except the upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyOptions {
    /// Forward `Upgrade: websocket` (default on).
    pub websocket: bool,
    /// Response headers added to every response (`X-Frame-Options`...).
    pub headers: BTreeMap<String, String>,
    /// Read timeout in seconds.
    pub read_timeout_secs: u32,
    /// Show the "starting" page on 502 while an `autostart: on-visit` process boots
    /// (docs/06 §4.2). Set from the process, not by the user.
    pub start_on_visit: bool,
}

impl Default for ProxyOptions {
    fn default() -> Self {
        Self {
            websocket: true,
            headers: BTreeMap::new(),
            read_timeout_secs: DEFAULT_READ_TIMEOUT_SECS,
            start_on_visit: false,
        }
    }
}

/// A validated upstream: `http(s)://host[:port]`, no path, query or credentials (a path
/// would change what `proxy_pass` forwards, which the three servers disagree on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTarget {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

impl ProxyTarget {
    /// Parses and validates `text`.
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = |why: &str| ForgeError::InvalidInput(messages::upstream_invalid(text, why).1);
        let url = Url::parse(text.trim()).map_err(|_| invalid("URL malformada"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid("use http:// ou https://"));
        }
        let host = url.host_str().ok_or_else(|| invalid("faltou o host"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("credenciais na URL não são aceitas"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("sem query nem fragmento"));
        }
        if url.path() != "/" && !url.path().is_empty() {
            return Err(invalid("sem caminho: o site inteiro vai para o upstream"));
        }
        Ok(Self {
            scheme: url.scheme().to_owned(),
            host: host.to_owned(),
            port: url.port(),
        })
    }

    /// `http://127.0.0.1:3000` (IPv6 hosts keep their brackets).
    pub fn url(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{port}", self.scheme, self.host),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }
}

/// `http://127.0.0.1:<port>`: where a process of the apps range listens.
pub fn loopback_upstream(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The model of a proxy site: hosts, TLS and `extra` from the common part
/// ([`base_model`]), the [`ProxyModel`] and the response headers. Not yet normalized
/// (see [`crate::vhost::build`]).
pub fn proxy_vhost(
    site: &Site,
    ctx: &VhostContext,
    upstream: &str,
    options: &ProxyOptions,
) -> Result<VhostModel> {
    let target = ProxyTarget::parse(upstream)?;
    if options.read_timeout_secs == 0 {
        return Err(ForgeError::InvalidInput(
            "read_timeout_secs must be greater than zero".to_owned(),
        ));
    }
    let mut model = base_model(site, ctx, site.kind);
    model.kind = SiteKind::Proxy;
    model.headers = options.headers.clone();
    model.proxy = Some(ProxyModel {
        upstream: target.url(),
        websocket: options.websocket,
        read_timeout_secs: options.read_timeout_secs,
        start_on_visit: options.start_on_visit,
    });
    Ok(model)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forge_core::store::sites::SiteKind as StoredKind;
    use forge_render::ExtraModel;
    use pretty_assertions::assert_eq;

    use super::*;

    fn ctx() -> VhostContext {
        VhostContext {
            certs_sites: PathBuf::from("C:/Forgebox/certs/sites"),
            default_php_series: "8.3".to_owned(),
            extra: ExtraModel::default(),
            app_ports: BTreeMap::new(),
            proxy_options: BTreeMap::new(),
        }
    }

    fn site(upstream: &str) -> Site {
        let mut site = Site::new("app.test", "app-test");
        site.kind = StoredKind::Proxy;
        site.upstream = Some(upstream.to_owned());
        site
    }

    #[test]
    fn upstreams_are_validated_and_normalized() {
        assert_eq!(
            ProxyTarget::parse("http://127.0.0.1:3000/").unwrap().url(),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            ProxyTarget::parse("https://backend.internal")
                .unwrap()
                .url(),
            "https://backend.internal"
        );
        assert_eq!(
            ProxyTarget::parse("http://[::1]:5173").unwrap().url(),
            "http://[::1]:5173"
        );
        for bad in [
            "",
            "127.0.0.1:3000",
            "ftp://x:21",
            "http://",
            "http://u:p@x:1",
            "http://x:1/api",
            "http://x:1/?a=b",
            "http://x:1/#f",
            "http://x:99999",
        ] {
            assert_eq!(
                ProxyTarget::parse(bad).unwrap_err().code(),
                "invalid_input",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn proxy_model_carries_websocket_headers_and_tls() {
        let mut options = ProxyOptions::default();
        options
            .headers
            .insert("X-Frame-Options".to_owned(), "SAMEORIGIN".to_owned());
        let model = proxy_vhost(
            &site("http://127.0.0.1:3000"),
            &ctx(),
            "http://127.0.0.1:3000",
            &options,
        )
        .unwrap()
        .normalized()
        .unwrap();
        assert_eq!(model.kind, SiteKind::Proxy);
        assert_eq!(model.hosts, ["app.test", "*.app.test"]);
        let proxy = model.proxy.unwrap();
        assert_eq!(proxy.upstream, "http://127.0.0.1:3000");
        assert!(proxy.websocket && !proxy.start_on_visit);
        assert_eq!(proxy.read_timeout_secs, 3600);
        assert_eq!(model.headers["X-Frame-Options"], "SAMEORIGIN");
        let ssl = model.ssl.unwrap();
        assert_eq!(ssl.cert, "C:/Forgebox/certs/sites/app.test.crt");
        assert!(model.http_redirect);
    }

    #[test]
    fn websocket_can_be_switched_off() {
        let options = ProxyOptions {
            websocket: false,
            ..ProxyOptions::default()
        };
        let model = proxy_vhost(
            &site("http://127.0.0.1:3000"),
            &ctx(),
            "http://127.0.0.1:3000",
            &options,
        )
        .unwrap();
        assert!(!model.proxy.unwrap().websocket);
        let zero = ProxyOptions {
            read_timeout_secs: 0,
            ..ProxyOptions::default()
        };
        assert!(proxy_vhost(&site("http://x:1"), &ctx(), "http://x:1", &zero).is_err());
    }
}
