//! Post-processing shared by every provider: curated hashes, variant/channel/constraint
//! filters, de-duplication and the preferred-first order.

use std::collections::HashSet;

use forge_core::error::{ForgeError, Result};
use forge_manifest::version::{Build, Version, channel_matches, sort_builds};

use super::{Release, ResolveRequest};

/// Applies the request to the raw `releases` of a provider (see the module docs of
/// [`super`]). Releases whose version the manifest scheme cannot parse are dropped.
pub(crate) fn finish(req: &ResolveRequest<'_>, releases: Vec<Release>) -> Result<Vec<Release>> {
    let versioning = &req.tool.spec.versioning;
    let scheme = versioning.scheme;
    let channel = match req.channel {
        Some(name) => Some(versioning.channels.get(name).ok_or_else(|| {
            ForgeError::InvalidInput(format!(
                "tool `{}` has no channel `{name}`",
                req.tool.metadata.id
            ))
        })?),
        None => None,
    };
    let allow_prerelease = channel.is_some_and(|c| c.prerelease);
    let curated = &req.tool.spec.source.releases;

    let mut seen: HashSet<(String, Vec<(String, String)>)> = HashSet::new();
    let mut kept: Vec<(Version, Release)> = Vec::new();
    for mut release in releases {
        let version = match Version::parse(scheme, &release.version) {
            Ok(v) => v,
            Err(err) => {
                tracing::debug!(version = release.version, error = %err, "skipping unparsable version");
                continue;
            }
        };
        if let Some(channel) = channel {
            if !channel_matches(channel, scheme, &version).unwrap_or(false) {
                continue;
            }
        } else if version.is_prerelease() {
            continue;
        }
        if let Some(constraint) = req.constraint {
            if !constraint.matches_with(&version, allow_prerelease) {
                continue;
            }
        }
        if release.variants.iter().any(|(key, value)| {
            versioning
                .variant(key)
                .is_some_and(|def| !def.values.is_empty() && !def.values.contains(value))
        }) {
            continue;
        }
        if req
            .variants
            .iter()
            .any(|(key, value)| release.variants.get(key) != Some(value))
        {
            continue;
        }
        if release.sha256.is_none() {
            let hit = curated.iter().find(|c| {
                c.sha256.is_some()
                    && c.version == release.version
                    && c.variants
                        .iter()
                        .all(|(k, v)| release.variants.get(k) == Some(v))
            });
            if let Some(hit) = hit {
                release.sha256 = hit.sha256.as_ref().map(|s| s.as_str().to_owned());
                if release.size.is_none() {
                    release.size = hit.size;
                }
            }
        }
        let key = (
            release.version.clone(),
            release
                .variants
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        if seen.insert(key) {
            kept.push((version, release));
        }
    }
    sort_builds(&mut kept, &versioning.variants, |(version, release)| {
        Build {
            version,
            variants: &release.variants,
        }
    });
    Ok(kept.into_iter().map(|(_, release)| release).collect())
}
