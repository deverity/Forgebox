//! `type: apache-lounge` (docs/05 §1): apachelounge.com has no API, so the download
//! pages (`download/VS17/`, `download/VS18/`, one per entry of `builds`) are parsed.
//!
//! * Every `.zip` anchor whose file name matches `asset` becomes a release. The default
//!   `asset` is the `httpd-<ver>-<yymmdd>-win64-VS<nn>.zip` archive (case-insensitive:
//!   the site mixes `Win64`/`win64` and `VS17`/`vs17`); a custom `asset` (mod_fcgid...)
//!   is case-sensitive and must capture the version in a group named `version` (or
//!   group 1). Only `win64` files are kept.
//! * Variants: `compiler` from the file name (`vs17`, `vs18`) and `arch` = `x64`. Both
//!   pages are returned, so the UI offers VS17 and VS18 builds side by side.
//! * Each httpd release links a `.txt` with SHA1..SHA512 and an `.asc`; the `.txt` is
//!   read for the newest [`super::ProviderConfig::max_checksum_fetches`] releases.
//! * The site is slow-moving: the cache TTL protects it. If it cannot be reached and the
//!   manifest has `source.releases`, those are used instead (docs/05 §1 fallback).

use std::sync::OnceLock;

use forge_core::error::{ForgeError, Result};
use forge_manifest::schema::tool::ApacheLoungeProvider;
use futures::future::BoxFuture;
use regex::Regex;

use super::checksum::fill_checksums;
use super::{
    Ctx, Provider, Release, ResolveRequest, SigKind, SigRef, SigSubject, curated, file_name_of,
    filter, join_url,
};

const NAME: &str = "apache-lounge";
const DEFAULT_ASSET: &str =
    r"(?i)^httpd-(?P<version>\d+\.\d+\.\d+)-(?P<date>\d{6})-win64-vs\d+\.zip$";

pub(crate) struct ApacheLounge {
    ctx: Ctx,
    spec: ApacheLoungeProvider,
}

impl ApacheLounge {
    pub(crate) fn new(ctx: Ctx, spec: ApacheLoungeProvider) -> Self {
        Self { ctx, spec }
    }
}

fn anchor_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"<a href="(?P<href>[^"]+\.zip)""#).expect("static regex"))
}

fn compiler_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)-(vs\d+|vc\d+)\.zip$").expect("static regex"))
}

fn date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\d{1,2}) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s*'(\d{2})")
            .expect("static regex")
    })
}

/// `10 Dec '25` as a UTC timestamp at midnight.
fn parse_date(text: &str) -> Option<jiff::Timestamp> {
    let caps = date_re().captures(text)?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == &caps[2])?
        + 1;
    format!(
        "20{}-{month:02}-{:02}T00:00:00Z",
        &caps[3],
        caps[1].parse::<u32>().ok()?
    )
    .parse()
    .ok()
}

/// The next `n` characters of `text` (never splits a character).
fn window(text: &str, n: usize) -> &str {
    let end = text
        .char_indices()
        .nth(n)
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

fn parse_page(html: &str, base: &str, asset: &Regex) -> Vec<Release> {
    let mut out = Vec::new();
    for caps in anchor_re().captures_iter(html) {
        let href = &caps["href"];
        let file = file_name_of(href);
        let Some(asset_caps) = asset.captures(&file) else {
            continue;
        };
        let version = asset_caps
            .name("version")
            .or_else(|| asset_caps.get(1))
            .map(|m| m.as_str().to_owned());
        let Some(version) = version else {
            continue;
        };
        if !file.to_ascii_lowercase().contains("win64") {
            continue;
        }
        let mut release = Release::new(version, join_url(base, href));
        release.file_name = file.clone();
        if let Some(compiler) = compiler_re().captures(&file) {
            release
                .variants
                .insert("compiler".to_owned(), compiler[1].to_ascii_lowercase());
        }
        release.variants.insert("arch".to_owned(), "x64".to_owned());
        if let Some(date) = asset_caps.name("date") {
            release.tags.push(format!("build:{}", date.as_str()));
        }
        let after = caps.get(0).map_or(0, |m| m.end());
        release.published_at = parse_date(window(&html[after..], 300));
        if html.contains(&format!("href=\"{href}.txt\"")) {
            release.checksum_url = Some(format!("{}.txt", release.url));
        }
        if html.contains(&format!("href=\"{href}.asc\"")) {
            release.signature = Some(SigRef {
                kind: SigKind::Pgp,
                url: format!("{}.asc", release.url),
                subject: SigSubject::Archive,
            });
        }
        out.push(release);
    }
    out
}

impl Provider for ApacheLounge {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let pattern = self.spec.asset.as_deref().unwrap_or(DEFAULT_ASSET);
            let asset = Regex::new(pattern).map_err(|e| {
                ForgeError::ConfigInvalid(format!("invalid apache-lounge asset regex: {e}"))
            })?;
            let base = &self.ctx.config.endpoints.apache_lounge;
            let mut releases = Vec::new();
            let mut last_err = None;
            for build in &self.spec.builds {
                let url = join_url(base, &format!("download/{}/", build.to_ascii_uppercase()));
                match self.ctx.get(NAME, &url).await {
                    Ok(page) => releases.extend(parse_page(&page.text(), base, &asset)),
                    Err(err) => {
                        tracing::warn!(url, error = %err, "apache lounge page unavailable");
                        last_err = Some(err);
                    }
                }
            }
            if releases.is_empty() {
                if !req.tool.spec.source.releases.is_empty() {
                    let fallback = curated::from_manifest(req.tool, NAME, false)?;
                    return filter::finish(&req, fallback);
                }
                if let Some(err) = last_err {
                    return Err(err);
                }
            }
            let mut releases = filter::finish(&req, releases)?;
            fill_checksums(
                &self.ctx.fetcher,
                NAME,
                &mut releases,
                self.ctx.config.max_checksum_fetches,
            )
            .await;
            Ok(releases)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_of_the_site() {
        assert_eq!(
            parse_date("  10 Dec '25  11.884k").unwrap().to_string(),
            "2025-12-10T00:00:00Z"
        );
        assert_eq!(
            parse_date("13 Aug'24    169K").unwrap().to_string(),
            "2024-08-13T00:00:00Z"
        );
        assert!(parse_date("no date here").is_none());
    }

    #[test]
    fn window_does_not_split_characters() {
        assert_eq!(window("aé€b", 3), "aé€");
        assert_eq!(window("ab", 10), "ab");
    }
}
