//! Versions, constraints, channels, series and build ordering (docs/04 §1, docs/08 §5).
//!
//! # Schemes
//! - `semver`: parsed with the `semver` crate. `8` and `8.4` are read as `8.0.0`/`8.4.0`
//!   (upstreams often drop the patch). Pre-releases (`8.6.0-RC1`) sort before the release.
//! - `calver` and `string`: split into segments on `.`, `-`, `_`, `+`. Segments are
//!   compared one by one: both numeric → by value (`2024.9 < 2024.10`); both text →
//!   byte-wise lexicographic; numeric < text; a prefix sorts first (`1.2 < 1.2.1`). `calver`
//!   also requires the first segment to be numeric. There is no pre-release notion.
//!
//! # Constraints
//! Comparators separated by spaces or commas are ANDed; `||` separates alternatives.
//! - `semver`: every operator of the `semver` crate (`=`, `>`, `>=`, `<`, `<=`, `~`, `^`,
//!   `*`, `3.4.*`). A bare version is **exact** and a partial one matches its series
//!   (`8.4` = `8.4.*`), unlike Cargo, where a bare version means `^`.
//! - `calver`/`string`: `=`, `>`, `>=`, `<`, `<=` with the segment order above; a bare
//!   value or `X.*` matches that value and everything below it (`12.8` matches `12.8.0.6908`).
//!
//! Pre-releases follow the `semver` crate (they only match comparators of the same
//! `major.minor.patch` that carry a pre-release) unless the channel says
//! `prerelease: true`, in which case they are compared by plain precedence.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use semver::{Comparator, Op, VersionReq};

use crate::schema::common::VersionScheme;
use crate::schema::tool::{Channel, Variant};

/// Error parsing a version, constraint or series key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    #[error("invalid version {value:?}: {reason}")]
    Version { value: String, reason: String },
    #[error("invalid constraint {value:?}: {reason}")]
    Constraint { value: String, reason: String },
    #[error("invalid series_key {value:?}: {reason}")]
    SeriesKey { value: String, reason: String },
}

fn version_err(value: &str, reason: impl Into<String>) -> VersionError {
    VersionError::Version {
        value: value.to_owned(),
        reason: reason.into(),
    }
}

fn constraint_err(value: &str, reason: impl Into<String>) -> VersionError {
    VersionError::Constraint {
        value: value.to_owned(),
        reason: reason.into(),
    }
}

/// One segment of a `calver`/`string` version.
#[derive(Debug, Clone)]
struct Segment {
    text: String,
    num: Option<u64>,
}

impl Segment {
    fn new(text: &str) -> Self {
        let num = if text.bytes().all(|b| b.is_ascii_digit()) {
            text.parse().ok()
        } else {
            None
        };
        Self {
            text: text.to_owned(),
            num,
        }
    }
}

impl Ord for Segment {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.num, other.num) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => self.text.cmp(&other.text),
        }
    }
}

impl PartialOrd for Segment {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Segment {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Segment {}

fn segments(text: &str) -> Vec<Segment> {
    text.split(['.', '-', '_', '+']).map(Segment::new).collect()
}

#[derive(Debug, Clone)]
enum Repr {
    Semver(semver::Version),
    Segments(Vec<Segment>),
}

/// A version parsed according to a [`VersionScheme`]. Equality and order follow the
/// scheme (module docs); [`Version::as_str`] keeps the original text.
#[derive(Debug, Clone)]
pub struct Version {
    raw: String,
    repr: Repr,
}

/// Characters allowed in a version: it becomes part of a folder name (`bin/<id>/<version>`).
fn is_version_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')
}

impl Version {
    pub fn parse(scheme: VersionScheme, text: &str) -> Result<Self, VersionError> {
        if text.is_empty() {
            return Err(version_err(text, "empty"));
        }
        if let Some(c) = text.chars().find(|c| !is_version_char(*c)) {
            return Err(version_err(
                text,
                format!("character {c:?} is not allowed (use [0-9A-Za-z._+-])"),
            ));
        }
        let repr = match scheme {
            VersionScheme::Semver => Repr::Semver(parse_semver_lenient(text)?),
            VersionScheme::Calver => {
                let segs = segments(text);
                if segs.iter().any(|s| s.text.is_empty()) {
                    return Err(version_err(text, "empty segment"));
                }
                if segs[0].num.is_none() {
                    return Err(version_err(text, "calver must start with a number"));
                }
                Repr::Segments(segs)
            }
            VersionScheme::String => {
                let segs = segments(text);
                if segs.iter().any(|s| s.text.is_empty()) {
                    return Err(version_err(text, "empty segment"));
                }
                Repr::Segments(segs)
            }
        };
        Ok(Self {
            raw: text.to_owned(),
            repr,
        })
    }

    /// The text as written in the manifest or by the provider.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The semver form, for `semver` versions.
    pub fn as_semver(&self) -> Option<&semver::Version> {
        match &self.repr {
            Repr::Semver(v) => Some(v),
            Repr::Segments(_) => None,
        }
    }

    pub fn is_prerelease(&self) -> bool {
        matches!(&self.repr, Repr::Semver(v) if !v.pre.is_empty())
    }

    /// Component `index` (0 = major, 1 = minor, 2 = patch) as text.
    pub fn component(&self, index: usize) -> Option<String> {
        match &self.repr {
            Repr::Semver(v) => match index {
                0 => Some(v.major.to_string()),
                1 => Some(v.minor.to_string()),
                2 => Some(v.patch.to_string()),
                _ => None,
            },
            Repr::Segments(segs) => segs.get(index).map(|s| s.text.clone()),
        }
    }

    /// The series of this version under `series_key` (`"{major}.{minor}"` → `8.3`).
    pub fn series(&self, key: &str) -> Result<String, VersionError> {
        series_key(key, self)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.repr, &other.repr) {
            (Repr::Semver(a), Repr::Semver(b)) => a.cmp(b),
            (Repr::Segments(a), Repr::Segments(b)) => a.cmp(b),
            // Different schemes never meet in one tool; keep the order total anyway.
            _ => self.raw.cmp(&other.raw),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

/// `8` → `8.0.0`, `8.4` → `8.4.0`, `8.4.1-rc1` unchanged.
fn parse_semver_lenient(text: &str) -> Result<semver::Version, VersionError> {
    let split = text.find(['-', '+']).unwrap_or(text.len());
    let (core, rest) = text.split_at(split);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3 {
        return Err(version_err(
            text,
            "semver has at most 3 numeric components; use scheme `string` or `calver`",
        ));
    }
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(version_err(text, "components must be numbers"));
    }
    let mut full = parts.join(".");
    for _ in parts.len()..3 {
        full.push_str(".0");
    }
    full.push_str(rest);
    semver::Version::parse(&full).map_err(|e| version_err(text, e.to_string()))
}

/// Comparison operator of a `calver`/`string` constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegOp {
    /// Equal or below (prefix match).
    Prefix,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone)]
struct SegComparator {
    op: SegOp,
    segments: Vec<Segment>,
}

impl SegComparator {
    fn matches(&self, v: &[Segment]) -> bool {
        match self.op {
            SegOp::Prefix => v.len() >= self.segments.len() && v.starts_with(&self.segments),
            SegOp::Gt => v > self.segments.as_slice(),
            SegOp::Ge => v >= self.segments.as_slice(),
            SegOp::Lt => v < self.segments.as_slice(),
            SegOp::Le => v <= self.segments.as_slice(),
        }
    }
}

#[derive(Debug, Clone)]
enum Alternative {
    Semver(VersionReq),
    Segments(Vec<SegComparator>),
}

/// A parsed version constraint (module docs).
#[derive(Debug, Clone)]
pub struct Constraint {
    raw: String,
    alternatives: Vec<Alternative>,
}

const OPERATOR_CHARS: [char; 5] = ['=', '>', '<', '~', '^'];

/// Splits an alternative into comparators, gluing a lone operator to the next token
/// (`">= 7.4"` → `">=7.4"`).
fn comparator_tokens(alternative: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut pending_op = String::new();
    for token in alternative
        .split([' ', '\t', ','])
        .filter(|t| !t.is_empty())
    {
        if token.chars().all(|c| OPERATOR_CHARS.contains(&c)) {
            pending_op.push_str(token);
        } else {
            tokens.push(format!("{pending_op}{token}"));
            pending_op.clear();
        }
    }
    if !pending_op.is_empty() {
        tokens.push(pending_op);
    }
    tokens
}

impl Constraint {
    pub fn parse(scheme: VersionScheme, text: &str) -> Result<Self, VersionError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(constraint_err(text, "empty"));
        }
        let mut alternatives = Vec::new();
        for alternative in trimmed.split("||") {
            let tokens = comparator_tokens(alternative);
            if tokens.is_empty() {
                return Err(constraint_err(text, "empty alternative around `||`"));
            }
            alternatives.push(match scheme {
                VersionScheme::Semver => Alternative::Semver(parse_semver_req(text, &tokens)?),
                VersionScheme::Calver | VersionScheme::String => {
                    Alternative::Segments(parse_seg_req(text, &tokens)?)
                }
            });
        }
        Ok(Self {
            raw: text.to_owned(),
            alternatives,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Whether `version` satisfies the constraint (strict pre-release rule).
    pub fn matches(&self, version: &Version) -> bool {
        self.matches_with(version, false)
    }

    /// Like [`Constraint::matches`]; with `allow_prerelease`, pre-releases are compared by
    /// plain precedence (`>=8.6.0-0` matches `8.6.1-RC1`; `8.6` matches `8.6.0-RC1`).
    pub fn matches_with(&self, version: &Version, allow_prerelease: bool) -> bool {
        self.alternatives
            .iter()
            .any(|alt| match (alt, &version.repr) {
                (Alternative::Semver(req), Repr::Semver(v)) => {
                    if allow_prerelease && !v.pre.is_empty() {
                        req.comparators.iter().all(|c| comparator_matches(c, v))
                    } else {
                        req.matches(v)
                    }
                }
                (Alternative::Segments(cmps), Repr::Segments(segs)) => {
                    cmps.iter().all(|c| c.matches(segs))
                }
                _ => false,
            })
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

fn parse_semver_req(raw: &str, tokens: &[String]) -> Result<VersionReq, VersionError> {
    let mut parts = Vec::with_capacity(tokens.len());
    for token in tokens {
        let starts_with_op = token.starts_with(OPERATOR_CHARS);
        if starts_with_op || token == "*" || token.contains('*') {
            parts.push(token.clone());
        } else {
            // Bare version: exact (partial = its series), not Cargo's caret.
            parts.push(format!("={token}"));
        }
    }
    VersionReq::parse(&parts.join(", ")).map_err(|e| constraint_err(raw, e.to_string()))
}

fn parse_seg_req(raw: &str, tokens: &[String]) -> Result<Vec<SegComparator>, VersionError> {
    tokens
        .iter()
        .map(|token| {
            let (op, value) = if let Some(v) = token.strip_prefix(">=") {
                (SegOp::Ge, v)
            } else if let Some(v) = token.strip_prefix("<=") {
                (SegOp::Le, v)
            } else if let Some(v) = token.strip_prefix('>') {
                (SegOp::Gt, v)
            } else if let Some(v) = token.strip_prefix('<') {
                (SegOp::Lt, v)
            } else if let Some(v) = token.strip_prefix('=') {
                (SegOp::Prefix, v)
            } else if token.starts_with(['~', '^']) {
                return Err(constraint_err(
                    raw,
                    "`~` and `^` are only available for scheme semver",
                ));
            } else {
                (SegOp::Prefix, token.as_str())
            };
            let value = value.strip_suffix(".*").unwrap_or(value);
            if value == "*" && op == SegOp::Prefix {
                return Ok(SegComparator {
                    op,
                    segments: Vec::new(),
                });
            }
            if value.is_empty() || !value.chars().all(is_version_char) {
                return Err(constraint_err(raw, format!("invalid comparator {token:?}")));
            }
            let segs = segments(value);
            if segs.iter().any(|s| s.text.is_empty()) {
                return Err(constraint_err(raw, format!("invalid comparator {token:?}")));
            }
            Ok(SegComparator { op, segments: segs })
        })
        .collect()
}

/// Comparator evaluation without the "same major.minor.patch" pre-release rule.
fn comparator_matches(cmp: &Comparator, v: &semver::Version) -> bool {
    match cmp.op {
        Op::Exact | Op::Wildcard => matches_exact(cmp, v),
        Op::Greater => matches_greater(cmp, v),
        Op::GreaterEq => matches_exact(cmp, v) || matches_greater(cmp, v),
        Op::Less => matches_less(cmp, v),
        Op::LessEq => matches_exact(cmp, v) || matches_less(cmp, v),
        Op::Tilde => matches_tilde(cmp, v),
        Op::Caret => matches_caret(cmp, v),
        _ => cmp.matches(v),
    }
}

fn matches_exact(cmp: &Comparator, v: &semver::Version) -> bool {
    if v.major != cmp.major {
        return false;
    }
    if cmp.minor.is_some_and(|minor| v.minor != minor) {
        return false;
    }
    match cmp.patch {
        // A partial version matches its whole series, pre-releases included.
        None => true,
        Some(patch) => v.patch == patch && v.pre == cmp.pre,
    }
}

fn matches_greater(cmp: &Comparator, v: &semver::Version) -> bool {
    if v.major != cmp.major {
        return v.major > cmp.major;
    }
    let Some(minor) = cmp.minor else {
        return false;
    };
    if v.minor != minor {
        return v.minor > minor;
    }
    let Some(patch) = cmp.patch else {
        return false;
    };
    if v.patch != patch {
        return v.patch > patch;
    }
    v.pre > cmp.pre
}

fn matches_less(cmp: &Comparator, v: &semver::Version) -> bool {
    if v.major != cmp.major {
        return v.major < cmp.major;
    }
    let Some(minor) = cmp.minor else {
        return false;
    };
    if v.minor != minor {
        return v.minor < minor;
    }
    let Some(patch) = cmp.patch else {
        return false;
    };
    if v.patch != patch {
        return v.patch < patch;
    }
    v.pre < cmp.pre
}

fn matches_tilde(cmp: &Comparator, v: &semver::Version) -> bool {
    if v.major != cmp.major {
        return false;
    }
    if cmp.minor.is_some_and(|minor| v.minor != minor) {
        return false;
    }
    match cmp.patch {
        Some(patch) if v.patch != patch => v.patch > patch,
        Some(_) => v.pre >= cmp.pre,
        None => true,
    }
}

fn matches_caret(cmp: &Comparator, v: &semver::Version) -> bool {
    if v.major != cmp.major {
        return false;
    }
    let Some(minor) = cmp.minor else {
        return true;
    };
    let Some(patch) = cmp.patch else {
        return if cmp.major > 0 {
            v.minor >= minor
        } else {
            v.minor == minor
        };
    };
    if cmp.major > 0 {
        if v.minor != minor {
            return v.minor > minor;
        }
        if v.patch != patch {
            return v.patch > patch;
        }
    } else if minor > 0 {
        if v.minor != minor {
            return false;
        }
        if v.patch != patch {
            return v.patch > patch;
        }
    } else if v.minor != minor || v.patch != patch {
        return false;
    }
    v.pre >= cmp.pre
}

/// Whether `version` belongs to `channel` (constraint + `prerelease` flag).
pub fn channel_matches(
    channel: &Channel,
    scheme: VersionScheme,
    version: &Version,
) -> Result<bool, VersionError> {
    let constraint = Constraint::parse(scheme, &channel.constraint)?;
    if version.is_prerelease() && !channel.prerelease {
        return Ok(false);
    }
    Ok(constraint.matches_with(version, channel.prerelease))
}

/// Renders a `series_key` (`"{major}.{minor}"`) for `version`. Placeholders: `{major}`,
/// `{minor}`, `{patch}`; a missing component renders as `0`.
pub fn series_key(template: &str, version: &Version) -> Result<String, VersionError> {
    render_series_key(template, |index| {
        version.component(index).unwrap_or_else(|| "0".to_owned())
    })
}

/// Checks the placeholders of a `series_key` without a version.
pub fn validate_series_key(template: &str) -> Result<(), VersionError> {
    render_series_key(template, |_| String::new()).map(|_| ())
}

fn render_series_key(
    template: &str,
    component: impl Fn(usize) -> String,
) -> Result<String, VersionError> {
    let err = |reason: &str| VersionError::SeriesKey {
        value: template.to_owned(),
        reason: reason.to_owned(),
    };
    let mut out = String::new();
    let mut rest = template;
    let mut placeholders = 0;
    while let Some(start) = rest.find('{') {
        let literal = &rest[..start];
        if literal.contains('}') {
            return Err(err("unbalanced `}`"));
        }
        out.push_str(literal);
        let after = &rest[start + 1..];
        let end = after.find('}').ok_or_else(|| err("unclosed `{`"))?;
        let index = match &after[..end] {
            "major" => 0,
            "minor" => 1,
            "patch" => 2,
            _ => return Err(err("placeholders are {major}, {minor} and {patch}")),
        };
        out.push_str(&component(index));
        placeholders += 1;
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(err("unbalanced `}`"));
    }
    out.push_str(rest);
    if placeholders == 0 {
        return Err(err("needs at least one of {major}, {minor}, {patch}"));
    }
    Ok(out)
}

/// Folder name of a build (docs/08 §5): `<version>[-<variant>...]`, variants in `order`
/// (the manifest's `versioning.variants`). Missing or empty values are skipped, so tools
/// without variants get just the version: PHP `8.3.37-nts-vs17-x64`, Apache
/// `2.4.69-vs18-x64`, Nginx `1.27.3`.
pub fn canonical_name<S: AsRef<str>>(
    version: &str,
    variants: &BTreeMap<String, String>,
    order: &[S],
) -> String {
    let mut name = version.to_owned();
    for key in order {
        if let Some(value) = variants.get(key.as_ref()).filter(|v| !v.is_empty()) {
            name.push('-');
            name.push_str(value);
        }
    }
    name
}

/// Preference rank of `value` in `variant`: the `default` first (0), then `values` in
/// declaration order; unknown values last.
pub fn variant_rank(variant: &Variant, value: &str) -> usize {
    if variant.default.as_deref() == Some(value) {
        return 0;
    }
    variant
        .values
        .iter()
        .position(|v| v == value)
        .map_or(usize::MAX, |i| i + 1)
}

/// A build to order: version plus its variant values.
#[derive(Debug, Clone, Copy)]
pub struct Build<'a> {
    pub version: &'a Version,
    pub variants: &'a BTreeMap<String, String>,
}

/// Order of preference between builds (`Less` = `a` is preferred): newer version first;
/// for the same version, each variant in manifest order by [`variant_rank`] (`nts` before
/// `ts`, `vs17` before `vs16` when declared `[vs17, vs16, vc15]`); then by text, so the
/// order is total and stable.
pub fn cmp_builds(a: &Build<'_>, b: &Build<'_>, variants: &[Variant]) -> Ordering {
    b.version
        .cmp(a.version)
        .then_with(|| {
            variants
                .iter()
                .map(|variant| {
                    let rank = |build: &Build<'_>| {
                        build
                            .variants
                            .get(&variant.key)
                            .map_or(usize::MAX, |v| variant_rank(variant, v))
                    };
                    rank(a).cmp(&rank(b))
                })
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.variants.cmp(b.variants))
}

/// Sorts `items` with the preferred build first ([`cmp_builds`]).
pub fn sort_builds<T>(items: &mut [T], variants: &[Variant], build: impl Fn(&T) -> Build<'_>) {
    items.sort_by(|a, b| cmp_builds(&build(a), &build(b), variants));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semver(text: &str) -> Version {
        Version::parse(VersionScheme::Semver, text).unwrap()
    }

    fn req(text: &str) -> Constraint {
        Constraint::parse(VersionScheme::Semver, text).unwrap()
    }

    #[test]
    fn semver_versions_are_lenient_on_missing_components() {
        assert_eq!(semver("8.4"), semver("8.4.0"));
        assert_eq!(semver("22").as_semver().unwrap().to_string(), "22.0.0");
        assert!(semver("8.6.0-RC1").is_prerelease());
        assert!(semver("8.6.0-RC1") < semver("8.6.0"));
        assert!(semver("8.3.37") < semver("8.4.0"));
        assert_eq!(semver("8.4.1").as_str(), "8.4.1");
        for bad in ["", "8.4.1.2", "v8.4", "8..1", "8.4 ", "8/4", "x.y.z"] {
            assert!(
                Version::parse(VersionScheme::Semver, bad).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn segment_versions_compare_naturally() {
        let v = |s: &str| Version::parse(VersionScheme::Calver, s).unwrap();
        assert!(v("2024.9") < v("2024.10"));
        assert!(v("2024.10") < v("2024.10.1"));
        assert_eq!(v("2024.01"), v("2024.1"));
        assert!(Version::parse(VersionScheme::Calver, "v2024").is_err());
        let s = |s: &str| Version::parse(VersionScheme::String, s).unwrap();
        assert!(s("12.8.0.6908") > s("12.7.1.6900"));
        assert!(s("2.47.1.windows.1") < s("2.47.1.windows.2"));
        assert!(s("1.0.0") < s("1.0.beta"), "numeric < text");
        assert!(Version::parse(VersionScheme::String, "1..2").is_err());
    }

    #[test]
    fn semver_constraints_from_docs() {
        let stable = req(">=7.4 <9");
        assert!(stable.matches(&semver("7.4.33")));
        assert!(stable.matches(&semver("8.4.14")));
        assert!(!stable.matches(&semver("9.0.0")));
        assert!(!stable.matches(&semver("7.3.33")));

        let tilde = req("~8.3");
        assert!(tilde.matches(&semver("8.3.37")));
        assert!(!tilde.matches(&semver("8.4.0")));

        let wildcard = req("3.4.*");
        assert!(wildcard.matches(&semver("3.4.1")));
        assert!(!wildcard.matches(&semver("3.5.0")));

        let bare = req("8.4");
        assert!(bare.matches(&semver("8.4.14")));
        assert!(!bare.matches(&semver("8.5.0")), "bare is exact, not caret");
        assert!(req("8.4.1").matches(&semver("8.4.1")));
        assert!(!req("8.4.1").matches(&semver("8.4.2")));

        let spaced = req(">= 8.1, < 8.3 || ^8.4");
        assert!(spaced.matches(&semver("8.2.0")));
        assert!(spaced.matches(&semver("8.5.0")));
        assert!(!spaced.matches(&semver("8.3.0")));
        assert!(req("*").matches(&semver("1.2.3")));

        for bad in ["", "||", ">=", "~>1.2", "8.x.y", ">=1.2 ||"] {
            assert!(
                Constraint::parse(VersionScheme::Semver, bad).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn prerelease_needs_channel_permission() {
        let preview = Channel {
            constraint: ">=8.6.0-0".into(),
            prerelease: true,
        };
        let stable = Channel {
            constraint: ">=7.4 <9".into(),
            prerelease: false,
        };
        let rc = semver("8.6.0-RC1");
        let rc_next = semver("8.6.1-RC1");
        assert!(channel_matches(&preview, VersionScheme::Semver, &rc).unwrap());
        assert!(channel_matches(&preview, VersionScheme::Semver, &rc_next).unwrap());
        assert!(!req(">=8.6.0-0").matches(&rc_next), "strict semver rule");
        assert!(!channel_matches(&stable, VersionScheme::Semver, &rc).unwrap());
        assert!(channel_matches(&stable, VersionScheme::Semver, &semver("8.4.1")).unwrap());
        assert!(req("8.6").matches_with(&rc, true));
        assert!(!req("8.6").matches(&rc));
    }

    #[test]
    fn segment_constraints() {
        let c = |s: &str| Constraint::parse(VersionScheme::String, s).unwrap();
        let v = |s: &str| Version::parse(VersionScheme::String, s).unwrap();
        assert!(c("12.8").matches(&v("12.8.0.6908")));
        assert!(c("12.8.*").matches(&v("12.8.0.6908")));
        assert!(!c("12.8").matches(&v("12.9.0")));
        assert!(c(">=12.7 <13").matches(&v("12.8.0.6908")));
        assert!(!c(">12.8.0.6908").matches(&v("12.8.0.6908")));
        assert!(c("<=12.8.0.6908").matches(&v("12.8.0.6908")));
        assert!(c("*").matches(&v("1")));
        assert!(Constraint::parse(VersionScheme::Calver, "~2024").is_err());
        assert!(Constraint::parse(VersionScheme::String, ">=").is_err());
        assert!(!c("12").matches(&semver("12.0.0")), "schemes never mix");
    }

    #[test]
    fn series_keys() {
        assert_eq!(semver("8.3.37").series("{major}.{minor}").unwrap(), "8.3");
        assert_eq!(semver("22.12.0").series("{major}").unwrap(), "22");
        let cal = Version::parse(VersionScheme::Calver, "2024.10.1").unwrap();
        assert_eq!(cal.series("{major}.{minor}").unwrap(), "2024.10");
        let short = Version::parse(VersionScheme::Calver, "2024").unwrap();
        assert_eq!(short.series("{major}.{minor}").unwrap(), "2024.0");
        for bad in ["", "static", "{major", "{build}", "major}", "{major}}"] {
            assert!(validate_series_key(bad).is_err(), "{bad:?}");
        }
        validate_series_key("v{major}.{minor}.x").unwrap();
    }

    fn variants() -> Vec<Variant> {
        vec![
            Variant {
                key: "ts".into(),
                values: vec!["nts".into(), "ts".into()],
                default: Some("nts".into()),
                label: BTreeMap::new(),
                derive: None,
            },
            Variant {
                key: "compiler".into(),
                values: vec!["vs17".into(), "vs16".into(), "vc15".into()],
                default: None,
                label: BTreeMap::new(),
                derive: None,
            },
            Variant {
                key: "arch".into(),
                values: vec!["x64".into()],
                default: Some("x64".into()),
                label: BTreeMap::new(),
                derive: None,
            },
        ]
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn canonical_names_follow_docs_08() {
        let php = map(&[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")]);
        let order = ["ts", "compiler", "arch"];
        assert_eq!(
            canonical_name("8.3.37", &php, &order),
            "8.3.37-nts-vs17-x64"
        );
        let apache = map(&[("compiler", "vs18"), ("arch", "x64")]);
        assert_eq!(
            canonical_name("2.4.69", &apache, &["compiler", "arch"]),
            "2.4.69-vs18-x64"
        );
        let none: [&str; 0] = [];
        assert_eq!(canonical_name("1.27.3", &BTreeMap::new(), &none), "1.27.3");
        let partial = map(&[("ts", "ts"), ("arch", "")]);
        assert_eq!(canonical_name("8.4.1", &partial, &order), "8.4.1-ts");
    }

    #[test]
    fn builds_sort_newest_then_preferred_variants() {
        let variants = variants();
        let v8_4 = semver("8.4.14");
        let v8_3 = semver("8.3.37");
        let builds = [
            (
                &v8_3,
                map(&[("ts", "nts"), ("compiler", "vs16"), ("arch", "x64")]),
            ),
            (
                &v8_4,
                map(&[("ts", "ts"), ("compiler", "vs17"), ("arch", "x64")]),
            ),
            (
                &v8_4,
                map(&[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")]),
            ),
            (
                &v8_3,
                map(&[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")]),
            ),
            (
                &v8_3,
                map(&[("ts", "nts"), ("compiler", "vs15"), ("arch", "x64")]),
            ),
        ];
        let mut sorted: Vec<_> = builds.iter().collect();
        sort_builds(&mut sorted, &variants, |(version, variants)| Build {
            version,
            variants,
        });
        let names: Vec<String> = sorted
            .iter()
            .map(|(v, vars)| canonical_name(v.as_str(), vars, &["ts", "compiler", "arch"]))
            .collect();
        assert_eq!(
            names,
            [
                "8.4.14-nts-vs17-x64",
                "8.4.14-ts-vs17-x64",
                "8.3.37-nts-vs17-x64",
                "8.3.37-nts-vs16-x64",
                "8.3.37-nts-vs15-x64",
            ]
        );
        assert_eq!(variant_rank(&variants[0], "nts"), 0);
        assert_eq!(variant_rank(&variants[0], "ts"), 2);
        assert_eq!(variant_rank(&variants[1], "vs17"), 1);
        assert_eq!(variant_rank(&variants[1], "vs18"), usize::MAX);
    }
}
