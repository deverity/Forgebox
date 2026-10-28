//! `type: nginx-org` (docs/05 §1): parses `nginx.org/en/download.html`, which lists one
//! build per line: Mainline, Stable and Legacy versions, each with the Windows zip and
//! its detached PGP signature (`.zip.asc`).
//!
//! nginx.org publishes no checksums, so releases come with `sha256: None` and the
//! `signature` to verify with the embedded nginx keyring; a curated
//! `source.releases[].sha256` is merged in when the manifest has one (docs/04 rule 2).
//! Tags: `mainline`, `stable` or `legacy`, from the heading above the row.

use std::sync::OnceLock;

use forge_core::error::{ForgeError, Result};
use futures::future::BoxFuture;
use regex::Regex;

use super::{
    Ctx, Provider, Release, ResolveRequest, SigKind, SigRef, SigSubject, filter, join_url,
};

const NAME: &str = "nginx-org";

pub(crate) struct NginxOrg {
    ctx: Ctx,
}

impl NginxOrg {
    pub(crate) fn new(ctx: Ctx) -> Self {
        Self { ctx }
    }
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<h4>\s*(Mainline|Stable|Legacy)\s+versions?\s*</h4>").expect("static regex")
    })
}

fn zip_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"href="(?P<path>/download/nginx-(?P<version>\d+\.\d+\.\d+)\.zip)""#)
            .expect("static regex")
    })
}

fn parse(html: &str, base: &str) -> Vec<Release> {
    let headings: Vec<(usize, String)> = heading_re()
        .captures_iter(html)
        .filter_map(|c| {
            let m = c.get(0)?;
            Some((m.start(), c[1].to_ascii_lowercase()))
        })
        .collect();
    zip_re()
        .captures_iter(html)
        .filter_map(|caps| {
            let at = caps.get(0)?.start();
            let tag = headings
                .iter()
                .rev()
                .find(|(pos, _)| *pos < at)
                .map(|(_, tag)| tag.clone());
            let path = &caps["path"];
            let url = join_url(base, path);
            let mut release = Release::new(caps["version"].to_owned(), url.clone());
            if html.contains(&format!("href=\"{path}.asc\"")) {
                release.signature = Some(SigRef {
                    kind: SigKind::Pgp,
                    url: format!("{url}.asc"),
                    subject: SigSubject::Archive,
                });
            }
            release.tags.extend(tag);
            Some(release)
        })
        .collect()
}

impl Provider for NginxOrg {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let base = &self.ctx.config.endpoints.nginx_org;
            let url = join_url(base, "en/download.html");
            let html = self.ctx.get(NAME, &url).await?.text();
            let releases = parse(&html, base);
            if releases.is_empty() {
                return Err(ForgeError::InvalidInput(format!(
                    "{url}: no nginx Windows zip found (page layout changed?)"
                )));
            }
            filter::finish(&req, releases)
        })
    }
}
