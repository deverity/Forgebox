//! `type: http` (docs/05 §1): a URL template for everything without an API.
//!
//! * Versions come from `source.releases` (static; an entry may carry its own `url`) or,
//!   when that list is empty, from `version_source`: a `regex` (group 1 of every match) or
//!   a `json_path` over the JSON document. With a version source, one release per
//!   combination of the manifest's declared variant values is produced.
//! * Placeholders of `url` and `checksum.url`: `{version}`, `{series}` (`series_key`),
//!   `{major}`, `{minor}`, `{patch}`, every variant key (`{compiler}`), every `vars` name
//!   (the string mapped from the release's value of the variant the map is keyed by) and,
//!   only in `checksum.url`, `{url}` and `{file}`. Unknown placeholders are errors.
//! * JSONPath subset: `$`, `.key`, `['key']`, `[n]` and `[*]`; leaves must be strings or
//!   numbers.
//! * The hash comes from `releases[].sha256` or from the checksum document; without
//!   either the release has no sha256 (docs/04 rule 2 makes the manifest invalid then).

use std::collections::BTreeMap;

use forge_core::error::{ForgeError, Result};
use forge_manifest::schema::tool::{HttpProvider, StaticRelease, VersionSource};
use forge_manifest::version::{Version, series_key};
use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use super::checksum::fill_checksums;
use super::{Ctx, Provider, Release, ResolveRequest, file_name_of, filter};

const NAME: &str = "http";

pub(crate) struct Http {
    ctx: Ctx,
    spec: HttpProvider,
}

impl Http {
    pub(crate) fn new(ctx: Ctx, spec: HttpProvider) -> Self {
        Self { ctx, spec }
    }

    async fn versions_from_source(&self, source: &VersionSource) -> Result<Vec<String>> {
        let fetched = self.ctx.get(NAME, &source.url).await?;
        match (&source.regex, &source.json_path) {
            (Some(pattern), None) => {
                let re = Regex::new(pattern).map_err(|e| {
                    ForgeError::ConfigInvalid(format!("invalid version_source regex: {e}"))
                })?;
                if re.captures_len() < 2 {
                    return Err(ForgeError::ConfigInvalid(
                        "version_source regex needs one capture group".to_owned(),
                    ));
                }
                let text = fetched.text();
                Ok(dedupe(
                    re.captures_iter(&text)
                        .filter_map(|c| c.get(1).map(|m| m.as_str().to_owned())),
                ))
            }
            (None, Some(path)) => {
                let value: Value = fetched.json(&source.url)?;
                Ok(dedupe(select_json_path(&value, path)?.into_iter()))
            }
            _ => Err(ForgeError::ConfigInvalid(
                "version_source needs exactly one of `regex` and `json_path`".to_owned(),
            )),
        }
    }
}

/// One build to render: from `source.releases[]` or from a version source.
struct Candidate<'a> {
    version: String,
    variants: BTreeMap<String, String>,
    /// URL of a static release; the provider template otherwise.
    url: Option<String>,
    entry: Option<&'a StaticRelease>,
}

fn dedupe(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items.filter(|i| seen.insert(i.clone())).collect()
}

/// All combinations of the declared variant values (`[{}]` without variants).
fn variant_combinations(req: &ResolveRequest<'_>) -> Vec<BTreeMap<String, String>> {
    let mut combos = vec![BTreeMap::new()];
    for variant in &req.tool.spec.versioning.variants {
        if variant.values.is_empty() {
            continue;
        }
        let mut next = Vec::new();
        for combo in &combos {
            for value in &variant.values {
                let mut c = combo.clone();
                c.insert(variant.key.clone(), value.clone());
                next.push(c);
            }
        }
        combos = next;
    }
    combos
}

/// Substitutes `{name}` placeholders from `values`.
fn render(template: &str, values: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('}').ok_or_else(|| {
            ForgeError::ConfigInvalid(format!("unclosed `{{` in URL template `{template}`"))
        })?;
        let name = &after[..end];
        let value = values.get(name).ok_or_else(|| {
            ForgeError::ConfigInvalid(format!(
                "unknown placeholder `{{{name}}}` in URL template `{template}`"
            ))
        })?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Placeholder values of one build.
fn placeholders(
    req: &ResolveRequest<'_>,
    spec: &HttpProvider,
    version: &Version,
    variants: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("version".to_owned(), version.as_str().to_owned());
    for (index, name) in ["major", "minor", "patch"].into_iter().enumerate() {
        values.insert(
            name.to_owned(),
            version.component(index).unwrap_or_else(|| "0".to_owned()),
        );
    }
    if let Ok(series) = series_key(&req.tool.spec.versioning.series_key, version) {
        values.insert("series".to_owned(), series);
    }
    for (key, value) in variants {
        values.insert(key.clone(), value.clone());
    }
    for (name, map) in &spec.vars {
        let mapped = variants
            .values()
            .find_map(|value| map.get(value))
            .cloned()
            .unwrap_or_default();
        values.insert(name.clone(), mapped);
    }
    values
}

impl Provider for Http {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let scheme = req.tool.spec.versioning.scheme;
            let mut builds: Vec<Release> = Vec::new();
            let statics = &req.tool.spec.source.releases;
            let mut candidates: Vec<Candidate<'_>> = Vec::new();
            if !statics.is_empty() {
                for entry in statics {
                    candidates.push(Candidate {
                        version: entry.version.clone(),
                        variants: entry.variants.clone(),
                        url: entry.url.clone(),
                        entry: Some(entry),
                    });
                }
            } else if let Some(source) = &self.spec.version_source {
                let combos = variant_combinations(&req);
                for version in self.versions_from_source(source).await? {
                    for combo in &combos {
                        candidates.push(Candidate {
                            version: version.clone(),
                            variants: combo.clone(),
                            url: None,
                            entry: None,
                        });
                    }
                }
            }
            for Candidate {
                version: raw,
                variants,
                url: own_url,
                entry,
            } in candidates
            {
                let version = match Version::parse(scheme, &raw) {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::debug!(version = raw, error = %err, "skipping unparsable version");
                        continue;
                    }
                };
                let values = placeholders(&req, &self.spec, &version, &variants);
                let url = render(own_url.as_deref().unwrap_or(&self.spec.url), &values)?;
                let mut release = Release::new(raw, url.clone());
                release.variants = variants;
                if let Some(entry) = entry {
                    release.sha256 = entry.sha256.as_ref().map(|s| s.as_str().to_owned());
                    release.size = entry.size;
                    release.published_at = entry.published_at;
                }
                if release.sha256.is_none() {
                    if let Some(checksum) = &self.spec.checksum {
                        let mut with_file = values.clone();
                        with_file.insert("url".to_owned(), url.clone());
                        with_file.insert("file".to_owned(), file_name_of(&url));
                        release.checksum_url = Some(render(&checksum.url, &with_file)?);
                    }
                }
                builds.push(release);
            }
            let mut releases = filter::finish(&req, builds)?;
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

#[derive(Debug, PartialEq)]
enum Step {
    Key(String),
    Index(usize),
    All,
}

fn parse_json_path(path: &str) -> Result<Vec<Step>> {
    let bad = |why: &str| ForgeError::ConfigInvalid(format!("json_path `{path}`: {why}"));
    let rest = path
        .strip_prefix('$')
        .ok_or_else(|| bad("must start with `$`"))?;
    let chars: Vec<char> = rest.chars().collect();
    let mut steps = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '.' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && chars[end] != '.' && chars[end] != '[' {
                    end += 1;
                }
                if end == start {
                    return Err(bad("empty key"));
                }
                steps.push(Step::Key(chars[start..end].iter().collect()));
                i = end;
            }
            '[' => {
                let close = chars[i..]
                    .iter()
                    .position(|c| *c == ']')
                    .ok_or_else(|| bad("unclosed `[`"))?
                    + i;
                let inner: String = chars[i + 1..close].iter().collect();
                steps.push(if inner == "*" {
                    Step::All
                } else if let Ok(n) = inner.parse::<usize>() {
                    Step::Index(n)
                } else {
                    let key = inner.trim_matches(|c| c == '\'' || c == '"');
                    if key.is_empty() {
                        return Err(bad("empty key"));
                    }
                    Step::Key(key.to_owned())
                });
                i = close + 1;
            }
            _ => return Err(bad("unexpected character")),
        }
    }
    Ok(steps)
}

fn select_json_path(value: &Value, path: &str) -> Result<Vec<String>> {
    let mut current: Vec<&Value> = vec![value];
    for step in parse_json_path(path)? {
        let mut next = Vec::new();
        for v in current {
            match (&step, v) {
                (Step::Key(k), Value::Object(map)) => next.extend(map.get(k)),
                (Step::Index(n), Value::Array(items)) => next.extend(items.get(*n)),
                (Step::All, Value::Array(items)) => next.extend(items.iter()),
                (Step::All, Value::Object(map)) => next.extend(map.values()),
                _ => {}
            }
        }
        current = next;
    }
    Ok(current
        .into_iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn render_substitutes_and_rejects_unknown() {
        let v = values(&[("version", "1.2.3"), ("compiler", "vs17")]);
        assert_eq!(
            render("https://x/{version}/a-{compiler}.zip", &v).unwrap(),
            "https://x/1.2.3/a-vs17.zip"
        );
        assert!(render("https://x/{nope}", &v).is_err());
        assert!(render("https://x/{version", &v).is_err());
    }

    #[test]
    fn json_path_subset() {
        let doc: Value = serde_json::from_str(
            r#"{"releases":[{"v":"1.0"},{"v":"1.1"},{"v":2}],"latest":"1.1"}"#,
        )
        .unwrap();
        assert_eq!(
            select_json_path(&doc, "$.releases[*].v").unwrap(),
            ["1.0", "1.1", "2"]
        );
        assert_eq!(select_json_path(&doc, "$.latest").unwrap(), ["1.1"]);
        assert_eq!(
            select_json_path(&doc, "$['releases'][1].v").unwrap(),
            ["1.1"]
        );
        assert!(select_json_path(&doc, "$.missing[*]").unwrap().is_empty());
        assert!(select_json_path(&doc, "releases").is_err());
    }
}
