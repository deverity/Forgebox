//! Resolution of a written path against the real names on disk, **ignoring what the
//! file system would do**: every component is looked up in a case-exact listing of its
//! folder, so the result is the same on a case-sensitive or insensitive folder.
//!
//! A reference is split into [`Seg`]ments (keeping the byte range of each one inside
//! the written text, so a fix can rewrite exactly that part). The last segment can be
//! tried with several [`Candidate`] spellings (`./Header` → `Header.js`, `Header.ts`,
//! `Header/index.js`; `partials.nav` → `partials/nav.blade.php`; `@use 'vars'` →
//! `_vars.scss`).

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_core::fs::case::fold_case;

/// Case-exact listings of folders, cached for one scan.
#[derive(Debug, Default)]
pub(crate) struct DirIndex {
    cache: HashMap<PathBuf, Option<Arc<Vec<String>>>>,
}

impl DirIndex {
    /// Names in `dir` as stored on disk, sorted; `None` when `dir` is not a readable
    /// folder.
    pub(crate) fn list(&mut self, dir: &Path) -> Option<Arc<Vec<String>>> {
        if let Some(hit) = self.cache.get(dir) {
            return hit.clone();
        }
        let listing = std::fs::read_dir(dir).ok().map(|rd| {
            let mut names: Vec<String> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Arc::new(names)
        });
        self.cache.insert(dir.to_path_buf(), listing.clone());
        listing
    }
}

/// Role of a segment of a written path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegKind {
    Name,
    /// `..`
    Parent,
    /// `.` or an empty segment (`a//b`, leading or trailing separator).
    Skip,
}

/// One component of a written path and where it sits in the written text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seg {
    pub text: String,
    pub range: Range<usize>,
    pub kind: SegKind,
}

/// Splits `text` on any of `seps`, keeping the byte range of each segment.
pub(crate) fn split(text: &str, seps: &[char]) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if seps.contains(&c) {
            out.push(seg(text, start..i));
            start = i + c.len_utf8();
        }
    }
    out.push(seg(text, start..text.len()));
    out
}

fn seg(text: &str, range: Range<usize>) -> Seg {
    let s = &text[range.clone()];
    let kind = match s {
        "" | "." => SegKind::Skip,
        ".." => SegKind::Parent,
        _ => SegKind::Name,
    };
    Seg {
        text: s.to_owned(),
        range,
        kind,
    }
}

/// A way to spell the last segment on disk: `prefix + last + suffix`, optionally
/// followed by an implicit file inside it (`index.js`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub prefix: &'static str,
    pub suffix: &'static str,
    /// File looked up inside the folder named by the last segment.
    pub inner: Option<&'static str>,
}

impl Candidate {
    pub(crate) const EXACT: Candidate = Candidate {
        prefix: "",
        suffix: "",
        inner: None,
    };

    pub(crate) const fn suffix(suffix: &'static str) -> Self {
        Self {
            prefix: "",
            suffix,
            inner: None,
        }
    }

    pub(crate) const fn partial(suffix: &'static str) -> Self {
        Self {
            prefix: "_",
            suffix,
            inner: None,
        }
    }

    pub(crate) const fn index(file: &'static str) -> Self {
        Self {
            prefix: "",
            suffix: "",
            inner: Some(file),
        }
    }
}

/// Result of looking a written path up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// Every component exists with exactly the written spelling.
    Exact(PathBuf),
    /// Every component exists, at least one only ignoring case.
    Insensitive(Found),
    Missing,
}

/// A path found ignoring case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// The real path (on-disk spelling).
    pub target: PathBuf,
    /// The path the reference names (written spelling), same number of components.
    pub expected: PathBuf,
    /// On-disk name matched by each segment (`None` for `..`/`.`/empty).
    pub actual: Vec<Option<String>>,
    /// On-disk name of the implicit inner file, when the candidate has one.
    pub inner_actual: Option<String>,
    /// More than one entry matched a component ignoring case (case-sensitive folder
    /// with `Header.js` and `header.js`): the first one was taken.
    pub ambiguous: bool,
}

/// Looks `name` up in `dir`: exact first, then ignoring case.
fn find(index: &mut DirIndex, dir: &Path, name: &str) -> Option<(String, bool, bool)> {
    let listing = index.list(dir)?;
    if listing.binary_search_by(|n| n.as_str().cmp(name)).is_ok() {
        return Some((name.to_owned(), true, false));
    }
    let folded = fold_case(name);
    let mut matches = listing.iter().filter(|n| fold_case(n) == folded);
    let first = matches.next()?.clone();
    let ambiguous = matches.next().is_some();
    Some((first, false, ambiguous))
}

/// Resolves `segs` from `base` with `cand` applied to the last name segment. A path
/// naming a folder only counts when `dir_ok` (manifest entries such as PSR-4 roots) or
/// when it is written with a trailing separator.
pub(crate) fn lookup(
    index: &mut DirIndex,
    base: &Path,
    segs: &[Seg],
    cand: &Candidate,
    dir_ok: bool,
) -> Lookup {
    let last_name = segs.iter().rposition(|s| s.kind == SegKind::Name);
    let Some(last_name) = last_name else {
        return Lookup::Missing;
    };
    let mut cur = base.to_path_buf();
    let mut expected = base.to_path_buf();
    let mut actual = Vec::with_capacity(segs.len());
    let mut exact = true;
    let mut ambiguous = false;
    for (i, s) in segs.iter().enumerate() {
        match s.kind {
            SegKind::Skip => actual.push(None),
            SegKind::Parent => {
                if !cur.pop() || !expected.pop() {
                    return Lookup::Missing;
                }
                actual.push(None);
            }
            SegKind::Name => {
                let name = if i == last_name {
                    format!("{}{}{}", cand.prefix, s.text, cand.suffix)
                } else {
                    s.text.clone()
                };
                let Some((on_disk, is_exact, amb)) = find(index, &cur, &name) else {
                    return Lookup::Missing;
                };
                exact &= is_exact;
                ambiguous |= amb;
                cur.push(&on_disk);
                expected.push(&name);
                actual.push(Some(on_disk));
            }
        }
    }
    let mut inner_actual = None;
    if let Some(inner) = cand.inner {
        let Some((on_disk, is_exact, amb)) = find(index, &cur, inner) else {
            return Lookup::Missing;
        };
        exact &= is_exact;
        ambiguous |= amb;
        cur.push(&on_disk);
        expected.push(inner);
        inner_actual = Some(on_disk);
    }
    // A reference to a folder is only meaningful for the candidates that expect one.
    if cand.inner.is_none() && cur.is_dir() && !(dir_ok || allows_dir(segs)) {
        return Lookup::Missing;
    }
    if exact {
        Lookup::Exact(cur)
    } else {
        Lookup::Insensitive(Found {
            target: cur,
            expected,
            actual,
            inner_actual,
            ambiguous,
        })
    }
}

/// Folder references are accepted when the written path ends with a separator
/// (`app/`, composer PSR-4 roots) — signalled by a trailing empty segment.
fn allows_dir(segs: &[Seg]) -> bool {
    segs.last()
        .is_some_and(|s| s.kind == SegKind::Skip && s.text.is_empty())
}

/// The written text with every name segment replaced by its on-disk spelling, or `None`
/// when the difference is in a part the text does not spell (the extension added by a
/// candidate, the implicit `index.js`): then only renaming the file fixes it.
pub(crate) fn corrected_text(
    text: &str,
    segs: &[Seg],
    cand: &Candidate,
    found: &Found,
) -> Option<String> {
    if let (Some(inner), Some(on_disk)) = (cand.inner, &found.inner_actual) {
        if inner != on_disk {
            return None;
        }
    }
    let last_name = segs.iter().rposition(|s| s.kind == SegKind::Name)?;
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (i, s) in segs.iter().enumerate() {
        let Some(on_disk) = found.actual.get(i).cloned().flatten() else {
            continue;
        };
        let replacement = if i == last_name {
            let stem_chars = on_disk.chars().count();
            let pre = cand.prefix.chars().count();
            let suf = cand.suffix.chars().count();
            if stem_chars < pre + suf {
                return None;
            }
            let chars: Vec<char> = on_disk.chars().collect();
            let on_disk_prefix: String = chars[..pre].iter().collect();
            let on_disk_suffix: String = chars[stem_chars - suf..].iter().collect();
            if on_disk_prefix != cand.prefix || on_disk_suffix != cand.suffix {
                return None;
            }
            chars[pre..stem_chars - suf].iter().collect::<String>()
        } else {
            on_disk
        };
        out.push_str(&text[pos..s.range.start]);
        out.push_str(&replacement);
        pos = s.range.end;
    }
    out.push_str(&text[pos..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        std::fs::create_dir_all(r.join("views/Partials")).unwrap();
        std::fs::write(r.join("views/Header.php"), "").unwrap();
        std::fs::write(r.join("views/Partials/nav.blade.php"), "").unwrap();
        std::fs::create_dir_all(r.join("js/Components")).unwrap();
        std::fs::write(r.join("js/header.js"), "").unwrap();
        std::fs::write(r.join("js/Components/index.js"), "").unwrap();
        std::fs::write(r.join("js/upper.JS"), "").unwrap();
        t
    }

    #[test]
    fn split_keeps_ranges() {
        let segs = split("../Views//header.php", &['/']);
        let texts: Vec<_> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["..", "Views", "", "header.php"]);
        assert_eq!(segs[1].range, 3..8);
        assert_eq!(segs[0].kind, SegKind::Parent);
        assert_eq!(segs[2].kind, SegKind::Skip);
        let segs = split("partials.Nav", &['.', '/']);
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn exact_insensitive_and_missing() {
        let t = tree();
        let mut idx = DirIndex::default();
        let segs = split("views/Header.php", &['/']);
        assert!(matches!(
            lookup(&mut idx, t.path(), &segs, &Candidate::EXACT, false),
            Lookup::Exact(_)
        ));
        let text = "Views/header.php";
        let segs = split(text, &['/']);
        let Lookup::Insensitive(found) =
            lookup(&mut idx, t.path(), &segs, &Candidate::EXACT, false)
        else {
            panic!("expected a case mismatch");
        };
        assert_eq!(found.target, t.path().join("views").join("Header.php"));
        assert_eq!(found.expected, t.path().join("Views").join("header.php"));
        assert_eq!(
            corrected_text(text, &segs, &Candidate::EXACT, &found).as_deref(),
            Some("views/Header.php")
        );
        let segs = split("views/Footer.php", &['/']);
        assert_eq!(
            lookup(&mut idx, t.path(), &segs, &Candidate::EXACT, false),
            Lookup::Missing
        );
    }

    #[test]
    fn candidates_and_parent_segments() {
        let t = tree();
        let mut idx = DirIndex::default();
        let base = t.path().join("views/Partials");
        let text = "../../JS/Header";
        let segs = split(text, &['/']);
        let cand = Candidate::suffix(".js");
        let Lookup::Insensitive(found) = lookup(&mut idx, &base, &segs, &cand, false) else {
            panic!();
        };
        assert_eq!(found.target, t.path().join("js").join("header.js"));
        assert_eq!(
            corrected_text(text, &segs, &cand, &found).as_deref(),
            Some("../../js/header")
        );
        // the extension differs only in case: not expressible by editing
        let segs = split("./upper", &['/']);
        let Lookup::Insensitive(found) =
            lookup(&mut idx, &t.path().join("js"), &segs, &cand, false)
        else {
            panic!();
        };
        assert_eq!(corrected_text("./upper", &segs, &cand, &found), None);
        // implicit index file
        let segs = split("./components", &['/']);
        let cand = Candidate::index("index.js");
        let Lookup::Insensitive(found) =
            lookup(&mut idx, &t.path().join("js"), &segs, &cand, false)
        else {
            panic!();
        };
        assert_eq!(
            corrected_text("./components", &segs, &cand, &found).as_deref(),
            Some("./Components")
        );
    }

    #[test]
    fn folders_need_a_trailing_separator() {
        let t = tree();
        let mut idx = DirIndex::default();
        let segs = split("Views", &['/']);
        assert_eq!(
            lookup(&mut idx, t.path(), &segs, &Candidate::EXACT, false),
            Lookup::Missing
        );
        let segs = split("Views/", &['/']);
        assert!(matches!(
            lookup(&mut idx, t.path(), &segs, &Candidate::EXACT, false),
            Lookup::Insensitive(_)
        ));
    }
}
