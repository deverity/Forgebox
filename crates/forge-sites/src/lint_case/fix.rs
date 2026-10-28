//! Fixing mismatches found by [`super::scan`]: rename the file or edit the reference.
//!
//! **Strategy per item.** [`FixOptions::overrides`] (by issue id) wins; then
//! [`FixOptions::strategy`] when it is `rename` or `edit`; with `auto`:
//! - PSR-4 mismatches are renamed (the class name is the source of truth);
//! - a reference is fixed by **renaming** its target only when the difference is in the
//!   file name alone (no folder renames behind the user's back), no other reference
//!   reaches that file with its current spelling, and every mismatched reference to it
//!   asks for the same spelling;
//! - otherwise the reference is **edited** to the on-disk spelling.
//!
//! An item whose chosen strategy is not available (editing a reference whose difference
//! is in an extension the text does not spell, renaming a file under `vendor/`) falls
//! back to the other one. When two items ask to rename the same file to different
//! spellings, the first wins and the others are edited.
//!
//! **Order.** Renames go first, deepest component first, each in two steps through a
//! temporary name (`a` → `a.forgebox-case-<pid>` → `A`), refusing to replace an existing
//! entry (a case-sensitive folder may hold both spellings). The project is then scanned
//! again and the planned edits are taken from the new scan (a rename may already have
//! fixed them, or changed what they must say). Each edit checks that the bytes at its
//! position still are the reference it expects (otherwise the file changed since the
//! scan and that edit is skipped with an error); edits of one file are applied together,
//! keeping the encoding, the BOM and the line endings, and written atomically. A final
//! scan fills [`LintReport::remaining`].
//!
//! **Preview.** With [`FixOptions::dry_run`] nothing is written: every planned fix is in
//! [`CaseIssue::fix`] (`from`/`to`, and the line before/after for edits).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::fs::case::fold_case;
use serde::{Deserialize, Serialize};

use super::{
    CaseFix, CaseIssue, FixStrategy, IssueCategory, LintOptions, LintReport, RefKind, scan,
};

/// Default strategy of a fix run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyChoice {
    #[default]
    Auto,
    Rename,
    Edit,
}

/// What [`fix()`] does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixOptions {
    /// Plan only; write nothing.
    pub dry_run: bool,
    /// Strategy of the items without an override.
    pub strategy: StrategyChoice,
    /// Strategy per issue id.
    pub overrides: BTreeMap<String, FixStrategy>,
    /// Fix only these issue ids (all mismatches when `None`).
    pub only: Option<BTreeSet<String>>,
}

/// The components of `rel` (relative path) as strings.
fn components(rel: &Path) -> Vec<String> {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// Strategy picked by `auto` for `issue` (see the module docs).
fn auto(
    issue: &CaseIssue,
    report: &LintReport,
    wanted_by_target: &HashMap<PathBuf, BTreeSet<PathBuf>>,
) -> FixStrategy {
    if issue.kind == RefKind::Psr4 {
        return FixStrategy::Rename;
    }
    let Some(detail) = &issue.internal else {
        return FixStrategy::Edit;
    };
    let target = components(&detail.target_abs);
    let expected = components(&detail.expected_abs);
    let only_file_name = target.len() == expected.len()
        && target
            .iter()
            .zip(&expected)
            .take(target.len().saturating_sub(1))
            .all(|(a, b)| a == b);
    let referenced_exactly = report
        .exact_targets
        .get(&detail.target_abs)
        .is_some_and(|n| *n > 0);
    let unanimous = wanted_by_target
        .get(&detail.target_abs)
        .is_none_or(|set| set.len() == 1);
    if only_file_name && !referenced_exactly && unanimous {
        FixStrategy::Rename
    } else {
        FixStrategy::Edit
    }
}

/// Picks the strategy of every selected mismatch and fills its `fix` preview.
fn plan(report: &mut LintReport, options: &FixOptions) {
    let mut wanted_by_target: HashMap<PathBuf, BTreeSet<PathBuf>> = HashMap::new();
    for issue in report.mismatches() {
        if let Some(d) = &issue.internal {
            wanted_by_target
                .entry(d.target_abs.clone())
                .or_default()
                .insert(d.expected_abs.clone());
        }
    }
    let mut chosen: Vec<Option<FixStrategy>> = Vec::with_capacity(report.issues.len());
    for issue in &report.issues {
        let selected = issue.category == IssueCategory::Mismatch
            && !issue.fixes.is_empty()
            && options
                .only
                .as_ref()
                .is_none_or(|ids| ids.contains(&issue.id));
        if !selected {
            chosen.push(None);
            continue;
        }
        let wanted = options
            .overrides
            .get(&issue.id)
            .copied()
            .unwrap_or_else(|| match options.strategy {
                StrategyChoice::Rename => FixStrategy::Rename,
                StrategyChoice::Edit => FixStrategy::Edit,
                StrategyChoice::Auto => auto(issue, report, &wanted_by_target),
            });
        let strategy = if issue.fixes.contains(&wanted) {
            wanted
        } else {
            issue.fixes[0]
        };
        chosen.push(Some(strategy));
    }
    // One rename per file: later conflicting requests are edited instead.
    let mut renames: HashMap<PathBuf, PathBuf> = HashMap::new();
    for (issue, choice) in report.issues.iter().zip(chosen.iter_mut()) {
        let (Some(FixStrategy::Rename), Some(d)) = (*choice, &issue.internal) else {
            continue;
        };
        match renames.get(&d.target_abs) {
            None => {
                renames.insert(d.target_abs.clone(), d.expected_abs.clone());
            }
            Some(existing) if *existing == d.expected_abs => {}
            Some(_) => {
                *choice = issue
                    .fixes
                    .contains(&FixStrategy::Edit)
                    .then_some(FixStrategy::Edit);
            }
        }
    }
    for (issue, choice) in report.issues.iter_mut().zip(chosen) {
        let Some(strategy) = choice else {
            continue;
        };
        let Some(d) = &issue.internal else {
            continue;
        };
        issue.fix = Some(match strategy {
            FixStrategy::Rename => CaseFix {
                strategy,
                from: issue.target.clone().unwrap_or_default(),
                to: issue.expected.clone().unwrap_or_default(),
                before: None,
                after: None,
                applied: false,
                error: None,
            },
            FixStrategy::Edit => {
                let corrected = d.corrected.clone().unwrap_or_default();
                let after = d.line_text.get(..d.line_offset).map(|head| {
                    let tail = d
                        .line_text
                        .get(d.line_offset + issue.reference.len()..)
                        .unwrap_or("");
                    format!("{head}{corrected}{tail}")
                });
                CaseFix {
                    strategy,
                    from: issue.reference.clone(),
                    to: corrected,
                    before: Some(d.line_text.clone()),
                    after,
                    applied: false,
                    error: None,
                }
            }
        });
    }
}

/// Current on-disk names of the path `expected` (relative to `root`), looked up ignoring
/// case component by component.
fn current_names(root: &Path, expected: &[String]) -> Result<Vec<String>> {
    let mut cur = root.to_path_buf();
    let mut out = Vec::with_capacity(expected.len());
    for name in expected {
        let mut names: Vec<String> = std::fs::read_dir(&cur)
            .at("listing", &cur)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let found = if names.iter().any(|n| n == name) {
            name.clone()
        } else {
            let folded = fold_case(name);
            let matches: Vec<&String> = names.iter().filter(|n| fold_case(n) == folded).collect();
            match matches.as_slice() {
                [one] => (*one).clone(),
                [] => {
                    return Err(ForgeError::NotFound(format!(
                        "{} (looking for {name})",
                        cur.display()
                    )));
                }
                _ => {
                    return Err(ForgeError::Conflict(format!(
                        "{} holds several names matching {name} ignoring case",
                        cur.display()
                    )));
                }
            }
        };
        cur.push(&found);
        out.push(found);
    }
    Ok(out)
}

/// Renames `parent/from` to `parent/to` (names differing only by case) in two steps.
fn rename_case(parent: &Path, from: &str, to: &str) -> Result<()> {
    let exists_exactly = std::fs::read_dir(parent)
        .at("listing", parent)?
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy() == to);
    if exists_exactly {
        return Err(ForgeError::Conflict(format!(
            "{} already exists",
            parent.join(to).display()
        )));
    }
    let src = parent.join(from);
    let tmp = parent.join(format!("{from}.forgebox-case-{}", std::process::id()));
    if std::fs::symlink_metadata(&tmp).is_ok() {
        return Err(ForgeError::Conflict(format!("{} exists", tmp.display())));
    }
    std::fs::rename(&src, &tmp).at("renaming", &src)?;
    if let Err(e) = std::fs::rename(&tmp, parent.join(to)) {
        let _ = std::fs::rename(&tmp, &src);
        return Err(ForgeError::io_at("renaming", &src, e));
    }
    Ok(())
}

/// Renames the file reached as `target_abs` so that its path reads `expected_abs`.
fn apply_rename(root: &Path, expected_abs: &Path) -> Result<()> {
    let rel = expected_abs.strip_prefix(root).map_err(|_| {
        ForgeError::InvalidInput(format!(
            "{} is outside the project; not renamed",
            expected_abs.display()
        ))
    })?;
    let expected = components(rel);
    let actual = current_names(root, &expected)?;
    for i in (0..expected.len()).rev() {
        if actual[i] != expected[i] {
            let parent = actual[..i]
                .iter()
                .fold(root.to_path_buf(), |p, c| p.join(c));
            rename_case(&parent, &actual[i], &expected[i])?;
        }
    }
    Ok(())
}

/// One planned edit: issue index, byte range, text expected there, replacement.
struct Edit {
    issue: usize,
    span: std::ops::Range<usize>,
    old: String,
    new: String,
}

/// Applies the edits of one file; returns the per-issue error of the ones skipped.
fn apply_edits(path: &Path, mut edits: Vec<Edit>) -> Vec<(usize, Option<String>)> {
    let text = match std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|b| String::from_utf8(b).map_err(|_| "not valid UTF-8".to_owned()))
    {
        Ok(text) => text,
        Err(err) => return edits.iter().map(|e| (e.issue, Some(err.clone()))).collect(),
    };
    edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
    let mut out = text.clone();
    let mut results = Vec::new();
    let mut last_start = usize::MAX;
    for edit in edits {
        let current = text.get(edit.span.clone());
        if current != Some(edit.old.as_str()) || edit.span.end > last_start {
            results.push((
                edit.issue,
                Some("the file changed since the scan; scan again".to_owned()),
            ));
            continue;
        }
        out.replace_range(edit.span.clone(), &edit.new);
        last_start = edit.span.start;
        results.push((edit.issue, None));
    }
    if out != text {
        if let Err(err) = forge_render::managed::write_atomic(path, &out) {
            return results
                .into_iter()
                .map(|(i, _)| (i, Some(err.to_string())))
                .collect();
        }
    }
    results
}

/// Scans the project, fixes the selected mismatches (or plans them, with
/// [`FixOptions::dry_run`]) and returns the first scan annotated with what was done.
/// Blocking.
pub fn fix(
    opts: &LintOptions,
    options: &FixOptions,
    progress: &mut dyn FnMut(u32),
    cancel: &dyn Fn() -> bool,
) -> Result<LintReport> {
    let mut report = scan(opts, progress, cancel)?;
    report.fix = true;
    report.dry_run = options.dry_run;
    plan(&mut report, options);
    if options.dry_run {
        return Ok(report);
    }
    let root = std::path::absolute(&opts.root).at("resolving", &opts.root)?;

    // 1. renames
    let mut renamed = false;
    for issue in &mut report.issues {
        let (Some(fix), Some(d)) = (issue.fix.as_mut(), issue.internal.as_ref()) else {
            continue;
        };
        if fix.strategy != FixStrategy::Rename {
            continue;
        }
        match apply_rename(&root, &d.expected_abs) {
            Ok(()) => {
                fix.applied = true;
                renamed = true;
            }
            Err(err) => fix.error = Some(err.to_string()),
        }
    }

    // 2. edits, from a fresh scan when files moved
    let fresh = if renamed {
        Some(scan(opts, &mut |_| {}, cancel)?)
    } else {
        None
    };
    let by_id: HashMap<&str, &CaseIssue> = fresh
        .as_ref()
        .map(|f| f.mismatches().map(|i| (i.id.as_str(), i)).collect())
        .unwrap_or_default();
    let mut per_file: BTreeMap<PathBuf, Vec<Edit>> = BTreeMap::new();
    for (idx, issue) in report.issues.iter_mut().enumerate() {
        if issue.fix.as_ref().map(|f| f.strategy) != Some(FixStrategy::Edit) {
            continue;
        }
        let source: Option<&CaseIssue> = if fresh.is_some() {
            by_id.get(issue.id.as_str()).copied()
        } else {
            Some(&*issue)
        };
        // (file, span, text there now, replacement) of the edit, if it can be made.
        let planned = source.map(|s| {
            s.internal.as_ref().and_then(|d| {
                Some((
                    d.file_abs.clone(),
                    d.span.clone()?,
                    s.reference.clone(),
                    d.corrected.clone()?,
                ))
            })
        });
        let Some(fix) = issue.fix.as_mut() else {
            continue;
        };
        match planned {
            None => {
                // Gone from the new scan: a rename already made this reference right.
                fix.applied = true;
                fix.to = fix.from.clone();
            }
            Some(None) => fix.error = Some("the reference cannot be edited".to_owned()),
            Some(Some((file, span, old, new))) => {
                fix.to = new.clone();
                per_file.entry(file).or_default().push(Edit {
                    issue: idx,
                    span,
                    old,
                    new,
                });
            }
        }
    }
    for (path, edits) in per_file {
        for (idx, error) in apply_edits(&path, edits) {
            if let Some(fix) = report.issues[idx].fix.as_mut() {
                fix.applied = error.is_none();
                fix.error = error;
            }
        }
    }
    for issue in &mut report.issues {
        issue.fixed = issue.fix.as_ref().is_some_and(|f| f.applied);
    }

    // 3. what is left
    let after = scan(opts, &mut |_| {}, cancel)?;
    report.remaining = Some(u32::try_from(after.mismatches().count()).unwrap_or(u32::MAX));
    Ok(report)
}
