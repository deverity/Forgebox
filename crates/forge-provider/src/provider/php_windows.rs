//! `type: php-windows` (docs/05 §1): `releases/releases.json` of windows.php.net for the
//! current series, the `releases/archives/` listing for older versions and, only for
//! channels with `prerelease`, the `qa/` listing.
//!
//! * Variants: `ts` (`ts`/`nts`), `compiler` (`vc15`, `vs16`, `vs17`...) and `arch`
//!   (`x64`; x86 builds are ignored, Forgebox targets `windows-x64`). All of them are
//!   returned; the manifest's variant order/default ranks them.
//! * Versions: PHP rebuilds publish `8.3.36-1`; as semver that would be a pre-release
//!   older than `8.3.36`, so it is normalized to `8.3.36+1` (build metadata). QA names
//!   `8.1.27RC1` become `8.1.27-RC1`. The real file name is kept in `file_name`.
//! * Sizes in `releases.json` are rounded text (`"32.75MB"`) and are not reported.
//! * `releases.json` carries sha256. The archive and QA listings do not (only a
//!   `sha1sum.txt`, observed 26/10/2026), so those releases have `sha256: None` unless
//!   the manifest curates them in `source.releases`.

use std::sync::OnceLock;

use forge_core::error::{ForgeError, Result};
use forge_manifest::schema::tool::PhpWindowsProvider;
use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use super::{Ctx, Provider, Release, ResolveRequest, filter, join_url};

const NAME: &str = "php-windows";

pub(crate) struct PhpWindows {
    ctx: Ctx,
    spec: PhpWindowsProvider,
}

impl PhpWindows {
    pub(crate) fn new(ctx: Ctx, spec: PhpWindowsProvider) -> Self {
        Self { ctx, spec }
    }
}

fn file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^php-(?P<core>\d+\.\d+\.\d+)(?P<rest>-\d+|-dev|RC\d+|alpha\d+|beta\d+)?(?P<nts>-nts)?-Win32-(?P<cc>vc\d+|vs\d+)-(?P<arch>x64)\.zip$",
        )
        .expect("static regex")
    })
}

fn variant_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?P<ts>nts|ts)-(?P<cc>vc\d+|vs\d+)-(?P<arch>x64)$").expect("static regex")
    })
}

fn row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"<a href="(?P<file>[^"/?]+\.zip)">[^<]*</a></td><td align="right">(?P<date>\d{4}-\d{2}-\d{2} \d{2}:\d{2})"#,
        )
        .expect("static regex")
    })
}

/// `(normalized version, [ts, compiler, arch])` of a published file name.
fn parse_file_name(name: &str) -> Option<(String, [String; 3])> {
    let caps = file_re().captures(name)?;
    let mut version = caps["core"].to_owned();
    if let Some(rest) = caps.name("rest") {
        let rest = rest.as_str();
        match rest.strip_prefix('-') {
            Some(n) if n.bytes().all(|b| b.is_ascii_digit()) => {
                version.push('+');
                version.push_str(n);
            }
            Some(_) => version.push_str(rest),
            None => {
                version.push('-');
                version.push_str(rest);
            }
        }
    }
    let ts = if caps.name("nts").is_some() {
        "nts"
    } else {
        "ts"
    };
    Some((
        version,
        [
            ts.to_owned(),
            caps["cc"].to_owned(),
            caps["arch"].to_owned(),
        ],
    ))
}

fn variant_map(ts: &str, compiler: &str, arch: &str) -> std::collections::BTreeMap<String, String> {
    [("ts", ts), ("compiler", compiler), ("arch", arch)]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn parse_listing_date(text: &str) -> Option<jiff::Timestamp> {
    let (date, time) = text.split_once(' ')?;
    format!("{date}T{time}:00Z").parse().ok()
}

/// Releases of an Apache-style directory listing (`archives/`, `qa/`).
fn parse_listing(html: &str, base_url: &str, tag: &str) -> Vec<Release> {
    row_re()
        .captures_iter(html)
        .filter_map(|caps| {
            let file = &caps["file"];
            let (version, [ts, compiler, arch]) = parse_file_name(file)?;
            let mut release = Release::new(version, join_url(base_url, file));
            release.variants = variant_map(&ts, &compiler, &arch);
            release.published_at = parse_listing_date(&caps["date"]);
            release.tags.push(tag.to_owned());
            Some(release)
        })
        .collect()
}

fn parse_releases_json(body: &Value, base_url: &str) -> Result<Vec<Release>> {
    let series = body.as_object().ok_or_else(|| {
        ForgeError::InvalidInput("releases.json: expected an object of series".to_owned())
    })?;
    let mut out = Vec::new();
    for (name, entry) in series {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if entry.get("version").and_then(Value::as_str).is_none() {
            tracing::debug!(series = name, "releases.json entry without `version`");
            continue;
        }
        for (key, build) in entry {
            let Some(caps) = variant_key_re().captures(key) else {
                continue;
            };
            let Some(zip) = build.get("zip") else {
                continue;
            };
            let (Some(path), Some(sha256)) = (
                zip.get("path").and_then(Value::as_str),
                zip.get("sha256").and_then(Value::as_str),
            ) else {
                continue;
            };
            let Some((version, _)) = parse_file_name(path) else {
                tracing::debug!(path, "releases.json: unexpected file name");
                continue;
            };
            let mut release = Release::new(version, join_url(base_url, path));
            release.sha256 = Some(sha256.to_ascii_lowercase());
            release.variants = variant_map(&caps["ts"], &caps["cc"], &caps["arch"]);
            release.published_at = build
                .get("mtime")
                .and_then(Value::as_str)
                .and_then(|t| t.parse().ok());
            release.tags.push("current".to_owned());
            out.push(release);
        }
    }
    Ok(out)
}

impl Provider for PhpWindows {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let base = &self.ctx.config.endpoints.php_windows;
            let releases_base = join_url(base, "releases");
            let json_url = join_url(&releases_base, "releases.json");
            let body: Value = self.ctx.get(NAME, &json_url).await?.json(&json_url)?;
            let mut releases = parse_releases_json(&body, &releases_base)?;

            // Older and QA listings are best effort: the current series are enough to work.
            let mut listings = Vec::new();
            if self.spec.include_archives {
                listings.push((join_url(&releases_base, "archives/"), "archive"));
            }
            let prerelease_channel = req
                .channel
                .and_then(|c| req.tool.spec.versioning.channels.get(c))
                .is_some_and(|c| c.prerelease);
            if self.spec.qa && prerelease_channel {
                listings.push((join_url(base, "qa/"), "qa"));
            }
            for (url, tag) in listings {
                match self.ctx.get(NAME, &url).await {
                    Ok(page) => releases.extend(parse_listing(&page.text(), &url, tag)),
                    Err(err) => tracing::warn!(url, error = %err, "php listing unavailable"),
                }
            }
            filter::finish(&req, releases)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_become_versions_and_variants() {
        let (v, vars) = parse_file_name("php-8.3.36-1-nts-Win32-vs16-x64.zip").unwrap();
        assert_eq!(v, "8.3.36+1");
        assert_eq!(vars, ["nts", "vs16", "x64"]);
        let (v, vars) = parse_file_name("php-8.4.1-Win32-vs17-x64.zip").unwrap();
        assert_eq!(v, "8.4.1");
        assert_eq!(vars[0], "ts");
        assert_eq!(
            parse_file_name("php-8.1.27RC1-nts-Win32-vs16-x64.zip")
                .unwrap()
                .0,
            "8.1.27-RC1"
        );
        assert_eq!(
            parse_file_name("php-8.3.34-dev-Win32-vs16-x64.zip")
                .unwrap()
                .0,
            "8.3.34-dev"
        );
        assert!(parse_file_name("php-8.4.1-Win32-vs17-x86.zip").is_none());
        assert!(parse_file_name("php-8.4.1-src.zip").is_none());
        assert!(parse_file_name("php-5.2.17-Win32-VC6-x86.zip").is_none());
    }

    #[test]
    fn listing_dates_are_utc() {
        assert_eq!(
            parse_listing_date("2024-12-17 20:00").unwrap().to_string(),
            "2024-12-17T20:00:00Z"
        );
    }
}
