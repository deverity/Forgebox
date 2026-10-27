//! Releases written by hand (or by the registry bot) in `source.releases[]`: the whole
//! source of `mysql` and `postgresql-edb` (no scraping, upstream has no usable API/hash),
//! the fallback of `apache-lounge` and the static list of `http`.

use forge_core::error::{ForgeError, Result};
use forge_manifest::schema::ToolManifest;
use url::Url;

use super::Release;

/// Converts `source.releases[]` into [`Release`]s. Every entry needs a `url` (http or
/// https); with `require_sha256` it also needs the curated `sha256` (docs/04 rule 2).
/// All problems are reported together.
pub(crate) fn from_manifest(
    tool: &ToolManifest,
    provider: &str,
    require_sha256: bool,
) -> Result<Vec<Release>> {
    let mut problems = Vec::new();
    let mut out = Vec::new();
    for entry in &tool.spec.source.releases {
        let label = format!("releases[{}]", entry.version);
        let Some(url) = &entry.url else {
            problems.push(format!(
                "{label}: `url` is required by provider `{provider}`"
            ));
            continue;
        };
        match Url::parse(url) {
            Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
            _ => {
                problems.push(format!("{label}: `{url}` is not an http(s) URL"));
                continue;
            }
        }
        if require_sha256 && entry.sha256.is_none() {
            problems.push(format!("{label}: `sha256` is required (docs/04 rule 2)"));
            continue;
        }
        let mut release = Release::new(entry.version.clone(), url.clone());
        release.sha256 = entry.sha256.as_ref().map(|s| s.as_str().to_owned());
        release.size = entry.size;
        release.variants = entry.variants.clone();
        release.published_at = entry.published_at;
        release.tags.push("curated".to_owned());
        out.push(release);
    }
    if problems.is_empty() {
        Ok(out)
    } else {
        Err(ForgeError::ConfigInvalid(format!(
            "tool `{}`: {}",
            tool.metadata.id,
            problems.join("; ")
        )))
    }
}
