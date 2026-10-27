//! What an update check finds (docs/05 §4): effective policy, update lines ("groups"),
//! the version that is active in each line and the best release within the policy's
//! reach. Pure functions over manifest data, `tool_versions` rows, `update_history` rows
//! and resolved releases; the engine does the I/O.
//!
//! * **Effective policy**: the user's override (`tools.update_policy`) or the manifest's
//!   `update.policy`; a tool of category `database` never goes past `notify` (`manual`
//!   stays `manual`), because data in `data/` needs a schema upgrade.
//! * **Group**: installed versions with the same series (`versioning.series_key`) and the
//!   same values for the variants the user chooses (variants with
//!   `derive: from_provider`, like the PHP compiler, are not part of the key: the
//!   provider decides them). `8.3.36-nts-vs16-x64` and `8.3.37-nts-vs16-x64` are one
//!   group; `8.3.37-ts-...` is another.
//! * **Active version** of a group (the one its pools serve): the target of the newest
//!   `completed` history row whose target is in the group; without one, the newest
//!   installed version that no `failed` update targeted. When that row is a rollback the
//!   group is *pinned*: nothing is applied automatically until the user updates it by
//!   hand. The daemon resolves a series to a pool version with [`active_versions`] too.
//! * **Reach**: `notify`/`manual` report what `auto-patch` would install (same series);
//!   `auto-patch` stays in the series; `auto-minor` follows minors, never a major;
//!   `auto` takes anything.
//! * A newer build whose provider-derived variants differ from the active one (a series
//!   that moved from vs16 to vs17) is reported **blocked** when the manifest migrates
//!   files (`update.migrate`): extensions copied from the old build may not load.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use forge_core::ids::{ToolId, VersionId};
use forge_core::store::{
    ToolVersionRow, UpdateHistoryRow, UpdateStatus, UpdateTrigger, VersionState,
};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::common::Category;
use forge_manifest::schema::tool::{UpdatePolicy, VariantDerive};
use forge_manifest::version::{Build, Version, canonical_name, cmp_builds};
use serde::{Deserialize, Serialize};

use crate::provider::Release;

/// How far an update goes from the active version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    /// Same series (`8.3.36 → 8.3.37`).
    Patch,
    /// Same major, another series (`1.28.0 → 1.30.0`).
    Minor,
    /// Another major.
    Major,
}

/// An update found by a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableUpdate {
    pub tool: ToolId,
    /// Series of the active version (`8.3`): the unit of rollback.
    pub series: String,
    /// Active version (`8.3.36`), its row and folder.
    pub from: String,
    pub from_id: VersionId,
    pub from_canonical: String,
    /// Target version (`8.3.37`) and its folder name.
    pub to: String,
    pub to_canonical: String,
    pub to_series: String,
    pub kind: UpdateKind,
    /// Effective policy of the tool.
    pub policy: UpdatePolicy,
    /// The policy lets the scheduler apply it without asking.
    pub automatic: bool,
    /// The group was rolled back: nothing is applied automatically until the user updates.
    pub pinned: bool,
    /// The target is already installed (after a rollback, or installed by hand).
    pub installed: bool,
    /// Why it cannot be applied (shown to the user); `None` when it can.
    pub blocked: Option<String>,
    /// What to download.
    pub release: Release,
}

/// The policy that applies: `override_policy` (`tools.update_policy`) or the manifest's,
/// capped at `notify` for databases.
pub fn effective_policy(manifest: &ToolManifest, override_policy: Option<&str>) -> UpdatePolicy {
    let chosen = override_policy
        .and_then(parse_policy)
        .unwrap_or(manifest.spec.update.policy);
    if manifest.metadata.category == Category::Database && chosen != UpdatePolicy::Manual {
        UpdatePolicy::Notify
    } else {
        chosen
    }
}

/// `manual|notify|auto-patch|auto-minor|auto`.
pub fn parse_policy(text: &str) -> Option<UpdatePolicy> {
    Some(match text {
        "manual" => UpdatePolicy::Manual,
        "notify" => UpdatePolicy::Notify,
        "auto-patch" => UpdatePolicy::AutoPatch,
        "auto-minor" => UpdatePolicy::AutoMinor,
        "auto" => UpdatePolicy::Auto,
        _ => return None,
    })
}

/// Text of a policy, as stored in `update_history.policy`.
pub fn policy_str(policy: UpdatePolicy) -> &'static str {
    match policy {
        UpdatePolicy::Manual => "manual",
        UpdatePolicy::Notify => "notify",
        UpdatePolicy::AutoPatch => "auto-patch",
        UpdatePolicy::AutoMinor => "auto-minor",
        UpdatePolicy::Auto => "auto",
    }
}

/// Whether the scheduler applies updates on its own under `policy`.
pub fn is_automatic(policy: UpdatePolicy) -> bool {
    matches!(
        policy,
        UpdatePolicy::AutoPatch | UpdatePolicy::AutoMinor | UpdatePolicy::Auto
    )
}

/// Farthest update reported under `policy`.
pub fn reach(policy: UpdatePolicy) -> UpdateKind {
    match policy {
        UpdatePolicy::Manual | UpdatePolicy::Notify | UpdatePolicy::AutoPatch => UpdateKind::Patch,
        UpdatePolicy::AutoMinor => UpdateKind::Minor,
        UpdatePolicy::Auto => UpdateKind::Major,
    }
}

/// Series of `version` under the manifest's `series_key` (the version itself when it
/// cannot be parsed).
pub fn series_of(manifest: &ToolManifest, version: &str) -> String {
    let versioning = &manifest.spec.versioning;
    Version::parse(versioning.scheme, version)
        .ok()
        .and_then(|v| v.series(&versioning.series_key).ok())
        .unwrap_or_else(|| version.to_owned())
}

/// Variant values the user chooses (not `derive: from_provider`).
fn chosen_variants(
    manifest: &ToolManifest,
    variants: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let derived: Vec<&str> = manifest
        .spec
        .versioning
        .variants
        .iter()
        .filter(|v| v.derive == Some(VariantDerive::FromProvider))
        .map(|v| v.key.as_str())
        .collect();
    variants
        .iter()
        .filter(|(k, _)| !derived.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Update line of a build: series + chosen variants.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupKey {
    pub series: String,
    pub variants: BTreeMap<String, String>,
}

impl GroupKey {
    pub fn of(manifest: &ToolManifest, version: &str, variants: &BTreeMap<String, String>) -> Self {
        Self {
            series: series_of(manifest, version),
            variants: chosen_variants(manifest, variants),
        }
    }

    pub fn of_row(manifest: &ToolManifest, row: &ToolVersionRow) -> Self {
        Self::of(manifest, &row.version, &row.variant)
    }

    /// Same chosen variants (any series).
    pub fn same_line(&self, manifest: &ToolManifest, variants: &BTreeMap<String, String>) -> bool {
        self.variants == chosen_variants(manifest, variants)
    }
}

/// How far `to` is from `from`.
pub fn classify(manifest: &ToolManifest, from: &Version, to: &Version) -> UpdateKind {
    let key = &manifest.spec.versioning.series_key;
    if from.series(key).ok() == to.series(key).ok() {
        UpdateKind::Patch
    } else if from.component(0) == to.component(0) {
        UpdateKind::Minor
    } else {
        UpdateKind::Major
    }
}

fn parse(manifest: &ToolManifest, text: &str) -> Option<Version> {
    Version::parse(manifest.spec.versioning.scheme, text).ok()
}

/// Preferred-first order of rows (newest, then variant preference).
pub fn cmp_rows(manifest: &ToolManifest, a: &ToolVersionRow, b: &ToolVersionRow) -> Ordering {
    match (parse(manifest, &a.version), parse(manifest, &b.version)) {
        (Some(va), Some(vb)) => cmp_builds(
            &Build {
                version: &va,
                variants: &a.variant,
            },
            &Build {
                version: &vb,
                variants: &b.variant,
            },
            &manifest.spec.versioning.variants,
        ),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.canonical_name.cmp(&b.canonical_name),
    }
}

/// The active version of every group of `rows` (module docs): `(group, active row,
/// pinned)`, groups in key order. Only `installed` rows of the manifest's tool count.
pub fn active_versions<'r>(
    manifest: &ToolManifest,
    rows: &'r [ToolVersionRow],
    history: &[UpdateHistoryRow],
) -> Vec<(GroupKey, &'r ToolVersionRow, bool)> {
    let mut groups: BTreeMap<GroupKey, Vec<&ToolVersionRow>> = BTreeMap::new();
    for row in rows.iter().filter(|r| {
        r.state == VersionState::Installed
            && r.tool_id == manifest.metadata.id
            && parse(manifest, &r.version).is_some()
    }) {
        groups
            .entry(GroupKey::of_row(manifest, row))
            .or_default()
            .push(row);
    }
    let mut out = Vec::new();
    for (key, mut members) in groups {
        members.sort_by(|a, b| cmp_rows(manifest, a, b));
        // `history` is newest first.
        let last = history.iter().find(|h| {
            h.status == UpdateStatus::Completed
                && h.to_version_id
                    .is_some_and(|id| members.iter().any(|m| m.id == id))
        });
        let (active, pinned) = match last {
            Some(h) => {
                let target = members
                    .iter()
                    .find(|m| Some(m.id) == h.to_version_id)
                    .copied()
                    .unwrap_or(members[0]);
                (target, h.triggered_by == UpdateTrigger::Rollback)
            }
            None => {
                // No switch recorded: the newest version no failed update targeted.
                let failed = |m: &&ToolVersionRow| {
                    history
                        .iter()
                        .any(|h| h.status == UpdateStatus::Failed && h.to_version_id == Some(m.id))
                };
                let target = members
                    .iter()
                    .find(|m| !failed(m))
                    .copied()
                    .unwrap_or(members[0]);
                (target, false)
            }
        };
        out.push((key, active, pinned));
    }
    out
}

/// Updates of `manifest`'s tool under `policy`, one per group at most (module docs).
/// `releases` are every stable release of the tool, any variant.
pub fn plan(
    manifest: &ToolManifest,
    policy: UpdatePolicy,
    rows: &[ToolVersionRow],
    history: &[UpdateHistoryRow],
    releases: &[Release],
) -> Vec<AvailableUpdate> {
    let variants = &manifest.spec.versioning.variants;
    let order = manifest.spec.versioning.variant_order();
    let reach = reach(policy);
    let mut parsed: Vec<(Version, &Release)> = releases
        .iter()
        .filter(|r| r.sha256.is_some() || r.sha512.is_some() || r.checksum_url.is_some())
        .filter_map(|r| parse(manifest, &r.version).map(|v| (v, r)))
        .filter(|(v, _)| !v.is_prerelease())
        .collect();
    parsed.sort_by(|(va, ra), (vb, rb)| {
        cmp_builds(
            &Build {
                version: va,
                variants: &ra.variants,
            },
            &Build {
                version: vb,
                variants: &rb.variants,
            },
            variants,
        )
    });
    let mut out = Vec::new();
    for (key, active, pinned) in active_versions(manifest, rows, history) {
        let Some(active_version) = parse(manifest, &active.version) else {
            continue;
        };
        let in_reach: Vec<&(Version, &Release)> = parsed
            .iter()
            .filter(|(v, r)| {
                *v > active_version
                    && key.same_line(manifest, &r.variants)
                    && classify(manifest, &active_version, v) <= reach
            })
            .collect();
        let Some((best_version, _)) = in_reach.first() else {
            continue;
        };
        // Among the builds of the best version, the one with the active build's derived
        // variants (same compiler), if any.
        let same_version: Vec<&&(Version, &Release)> =
            in_reach.iter().filter(|(v, _)| v == best_version).collect();
        let same_build = same_version
            .iter()
            .find(|(_, r)| r.variants == active.variant)
            .or_else(|| same_version.first())
            .copied();
        let Some((version, release)) = same_build.copied() else {
            continue;
        };
        let blocked =
            if release.variants != active.variant && !manifest.spec.update.migrate.is_empty() {
                Some(crate::messages::update_blocked_build(
                    &active.canonical_name,
                    &canonical_name(&release.version, &release.variants, &order),
                ))
            } else {
                None
            };
        let to_canonical = canonical_name(&release.version, &release.variants, &order);
        let installed = rows.iter().any(|r| {
            r.state == VersionState::Installed
                && r.tool_id == manifest.metadata.id
                && r.canonical_name == to_canonical
        });
        let kind = classify(manifest, &active_version, version);
        out.push(AvailableUpdate {
            tool: manifest.metadata.id.clone(),
            series: key.series.clone(),
            from: active.version.clone(),
            from_id: active.id,
            from_canonical: active.canonical_name.clone(),
            to: release.version.clone(),
            to_canonical,
            to_series: series_of(manifest, &release.version),
            kind,
            policy,
            automatic: is_automatic(policy) && !pinned && blocked.is_none(),
            pinned,
            installed,
            blocked,
            release: (*release).clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::ids::{JobId, UpdateId};
    use forge_core::store::VersionSource;
    use forge_manifest::Manifest;

    fn manifest(category: &str, policy: &str, migrate: bool) -> ToolManifest {
        let migrate = if migrate {
            "    migrate:\n      - copy_dir: { from: \"ext/\", to: \"ext/\", only_missing: true }\n"
        } else {
            ""
        };
        let text = format!(
            r#"
apiVersion: forgebox.dev/v1
kind: Tool
metadata: {{ id: php, name: PHP, category: {category} }}
spec:
  platforms: [windows-x64]
  versioning:
    variants:
      - {{ key: ts, values: [nts, ts], default: nts }}
      - {{ key: compiler, values: [vs17, vs16], derive: from_provider }}
  source:
    providers:
      - type: php-windows
  update:
    policy: {policy}
{migrate}"#
        );
        match forge_manifest::load_str(&text).unwrap() {
            Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    fn variants(ts: &str, compiler: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ts".to_owned(), ts.to_owned()),
            ("compiler".to_owned(), compiler.to_owned()),
        ])
    }

    fn row(version: &str, ts: &str, compiler: &str) -> ToolVersionRow {
        ToolVersionRow {
            id: VersionId::new(),
            tool_id: ToolId::new("php").unwrap(),
            version: version.into(),
            canonical_name: format!("{version}-{ts}-{compiler}"),
            variant: variants(ts, compiler),
            path: format!("C:/fb/bin/php/{version}-{ts}-{compiler}"),
            state: VersionState::Installed,
            source: VersionSource::Registry,
        }
    }

    fn release(version: &str, ts: &str, compiler: &str) -> Release {
        let mut r = Release::new(
            version,
            format!("https://example.invalid/php-{version}.zip"),
        );
        r.sha256 = Some("0".repeat(64));
        r.variants = variants(ts, compiler);
        r
    }

    fn catalog() -> Vec<Release> {
        vec![
            release("8.3.37", "nts", "vs16"),
            release("8.3.37", "ts", "vs16"),
            release("8.3.36", "nts", "vs16"),
            release("8.4.14", "nts", "vs17"),
            release("9.0.0", "nts", "vs17"),
        ]
    }

    #[test]
    fn policies_reach_and_databases() {
        let m = manifest("runtime", "auto-patch", false);
        assert_eq!(effective_policy(&m, None), UpdatePolicy::AutoPatch);
        assert_eq!(effective_policy(&m, Some("manual")), UpdatePolicy::Manual);
        assert_eq!(effective_policy(&m, Some("bogus")), UpdatePolicy::AutoPatch);
        let db = manifest("database", "auto", false);
        assert_eq!(effective_policy(&db, None), UpdatePolicy::Notify);
        assert_eq!(
            effective_policy(&db, Some("auto-patch")),
            UpdatePolicy::Notify
        );
        assert_eq!(effective_policy(&db, Some("manual")), UpdatePolicy::Manual);
        for p in ["manual", "notify", "auto-patch", "auto-minor", "auto"] {
            assert_eq!(policy_str(parse_policy(p).unwrap()), p);
        }
    }

    #[test]
    fn plan_per_policy() {
        let rows = vec![row("8.3.36", "nts", "vs16")];
        let patch = plan(
            &manifest("runtime", "auto-patch", false),
            UpdatePolicy::AutoPatch,
            &rows,
            &[],
            &catalog(),
        );
        assert_eq!(patch.len(), 1);
        let u = &patch[0];
        assert_eq!(
            (u.from.as_str(), u.to.as_str(), u.series.as_str()),
            ("8.3.36", "8.3.37", "8.3")
        );
        assert_eq!(u.to_canonical, "8.3.37-nts-vs16");
        assert_eq!(
            u.release.variants,
            variants("nts", "vs16"),
            "same chosen variant"
        );
        assert!(u.automatic && !u.installed && u.blocked.is_none());
        assert_eq!(u.kind, UpdateKind::Patch);

        let m = manifest("runtime", "auto-minor", false);
        let minor = plan(&m, UpdatePolicy::AutoMinor, &rows, &[], &catalog());
        assert_eq!(minor[0].to, "8.4.14");
        assert_eq!(minor[0].kind, UpdateKind::Minor);
        // The compiler is derived: a different one is fine without migrate.
        assert!(minor[0].blocked.is_none());

        let all = plan(&m, UpdatePolicy::Auto, &rows, &[], &catalog());
        assert_eq!(
            (all[0].to.as_str(), all[0].kind),
            ("9.0.0", UpdateKind::Major)
        );

        let notify = plan(&m, UpdatePolicy::Notify, &rows, &[], &catalog());
        assert_eq!(notify[0].to, "8.3.37");
        assert!(!notify[0].automatic);

        // Up to date: nothing.
        let latest = vec![row("8.3.37", "nts", "vs16")];
        assert!(plan(&m, UpdatePolicy::AutoPatch, &latest, &[], &catalog()).is_empty());
    }

    #[test]
    fn groups_by_chosen_variant_and_blocks_a_new_build_with_migrations() {
        let m = manifest("runtime", "auto-patch", true);
        let rows = vec![row("8.3.36", "nts", "vs16"), row("8.3.35", "ts", "vs16")];
        let releases = vec![
            release("8.3.37", "nts", "vs17"),
            release("8.3.37", "ts", "vs16"),
        ];
        let found = plan(&m, UpdatePolicy::AutoPatch, &rows, &[], &releases);
        assert_eq!(found.len(), 2, "{found:#?}");
        let nts = found.iter().find(|u| u.from == "8.3.36").unwrap();
        assert!(nts.blocked.is_some(), "vs16 -> vs17 with ext/ migration");
        assert!(!nts.automatic);
        let ts = found.iter().find(|u| u.from == "8.3.35").unwrap();
        assert_eq!(ts.to_canonical, "8.3.37-ts-vs16");
        assert!(ts.blocked.is_none() && ts.automatic);
    }

    #[test]
    fn rollback_pins_the_group() {
        let m = manifest("runtime", "auto-patch", false);
        let (old, new) = (row("8.3.36", "nts", "vs16"), row("8.3.37", "nts", "vs16"));
        let history = |trigger, to: VersionId| UpdateHistoryRow {
            id: UpdateId::new(),
            tool_id: ToolId::new("php").unwrap(),
            series: Some("8.3".into()),
            from_version_id: None,
            to_version_id: Some(to),
            from_version: None,
            to_version: String::new(),
            policy: None,
            triggered_by: trigger,
            status: UpdateStatus::Completed,
            error: None,
            job_id: Some(JobId::new()),
            rollback_of: None,
            started_at: jiff::Timestamp::now(),
            finished_at: None,
        };
        let rows = vec![old.clone(), new.clone()];
        let rolled_back = vec![history(UpdateTrigger::Rollback, old.id)];
        let active = active_versions(&m, &rows, &rolled_back);
        assert_eq!(active.len(), 1);
        assert_eq!((active[0].1.id, active[0].2), (old.id, true));
        let found = plan(&m, UpdatePolicy::AutoPatch, &rows, &rolled_back, &catalog());
        assert_eq!(found[0].to, "8.3.37");
        assert!(found[0].installed && found[0].pinned && !found[0].automatic);

        let updated = vec![history(UpdateTrigger::Auto, new.id)];
        let active = active_versions(&m, &rows, &updated);
        assert_eq!((active[0].1.id, active[0].2), (new.id, false));
    }
}
