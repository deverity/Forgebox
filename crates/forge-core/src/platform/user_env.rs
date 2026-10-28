//! The user's environment as other programs see it: the persistent user `PATH`
//! (docs/03 §5.2, docs/08 §3, ADR-0025).
//!
//! On Windows this is `HKCU\Environment\Path`, read and written as stored (a
//! `REG_EXPAND_SZ` keeps its `%VAR%` references unexpanded), followed by a
//! `WM_SETTINGCHANGE("Environment")` broadcast so that Explorer and new terminals pick
//! the change up ([`super::windows::WindowsUserEnv`]). Forgebox only ever adds or removes
//! whole entries of its own; every other entry, the order and even empty segments
//! (`a;;b;`) are written back byte for byte, so adding and then removing an entry restores
//! the original value.
//!
//! The list logic is platform neutral ([`PathValue`]) and shared by the real
//! implementation and [`MemoryUserEnv`], the in-memory stand-in used by tests and by
//! callers that must not touch the machine.
//!
//! All methods are **blocking** (registry access, a broadcast that waits for top-level
//! windows up to a timeout): call them from `spawn_blocking`.

use std::sync::Mutex;

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;

/// Separator of `PATH` lists on Windows.
pub const PATH_LIST_SEPARATOR: char = ';';

/// The persistent environment of the current user.
pub trait UserEnv: Send + Sync {
    /// Non-empty entries of the user `PATH`, as stored (unexpanded), in order. An absent
    /// value is an empty list.
    fn path_entries(&self) -> Result<Vec<String>>;

    /// Puts `entry` first in the user `PATH` unless an equivalent entry is already there
    /// ([`normalize_path_entry`]: case, `/` vs `\`, trailing separators and `%VAR%`
    /// references do not matter), and drops later duplicates of it. Returns whether the
    /// stored value changed; only then running programs are notified.
    fn add_path_entry(&self, entry: &str) -> Result<bool>;

    /// Removes every entry for which `predicate(raw_entry)` is true and returns them (in
    /// their former order). Running programs are notified when something was removed.
    fn remove_path_entries(&self, predicate: &dyn Fn(&str) -> bool) -> Result<Vec<String>>;
}

/// Replaces `%NAME%` references found by `lookup`; unknown ones are kept as written
/// (`cmd.exe` does the same).
pub fn expand_env_refs(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name) {
                    Some(value) => out.push_str(&value),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// [`expand_env_refs`] with the environment of the current process.
pub fn expand_with_process_env(text: &str) -> String {
    expand_env_refs(text, &|name| std::env::var(name).ok())
}

/// Comparable form of a `PATH` entry: `%VAR%` expanded with `lookup`, surrounding blanks
/// and quotes removed, `/` turned into `\`, trailing separators removed (a drive root
/// keeps its `\`), lowercase. Two entries naming the same folder compare equal.
pub fn normalize_path_entry(entry: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let expanded = expand_env_refs(entry.trim(), lookup);
    let mut text = expanded.trim().trim_matches('"').trim().replace('/', "\\");
    while text.ends_with('\\') && !(text.len() == 3 && text.as_bytes()[1] == b':') {
        text.pop();
    }
    text.to_lowercase()
}

/// A `PATH` value split into its raw segments (empty ones included), so that it is
/// written back exactly as it was apart from the entries Forgebox changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathValue {
    segments: Vec<String>,
}

impl PathValue {
    /// Splits `raw` on `;`. An empty value has no segments.
    pub fn parse(raw: &str) -> Self {
        if raw.is_empty() {
            return Self::default();
        }
        Self {
            segments: raw.split(PATH_LIST_SEPARATOR).map(str::to_owned).collect(),
        }
    }

    /// The value to store.
    pub fn to_raw(&self) -> String {
        self.segments.join(&PATH_LIST_SEPARATOR.to_string())
    }

    /// Non-empty entries, trimmed, in order.
    pub fn entries(&self) -> Vec<String> {
        self.segments
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// See [`UserEnv::add_path_entry`]. Returns whether the value changed.
    pub fn add(&mut self, entry: &str, lookup: &dyn Fn(&str) -> Option<String>) -> bool {
        let wanted = normalize_path_entry(entry, lookup);
        if wanted.is_empty() {
            return false;
        }
        let same = |segment: &String| {
            !segment.trim().is_empty() && normalize_path_entry(segment, lookup) == wanted
        };
        match self.segments.iter().position(same) {
            None => {
                self.segments.insert(0, entry.trim().to_owned());
                true
            }
            Some(first) => {
                let before = self.segments.len();
                let mut index = 0;
                self.segments.retain(|segment| {
                    let keep = index <= first || !same(segment);
                    index += 1;
                    keep
                });
                self.segments.len() != before
            }
        }
    }

    /// See [`UserEnv::remove_path_entries`]. Returns the removed entries (trimmed).
    pub fn remove_where(&mut self, predicate: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut removed = Vec::new();
        self.segments.retain(|segment| {
            let entry = segment.trim();
            if !entry.is_empty() && predicate(entry) {
                removed.push(entry.to_owned());
                false
            } else {
                true
            }
        });
        removed
    }
}

/// [`UserEnv`] kept in memory: a raw `PATH` value plus a count of the notifications that
/// a real implementation would have broadcast. For tests and dry runs.
#[derive(Debug, Default)]
pub struct MemoryUserEnv {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    raw: String,
    notifications: u32,
}

impl MemoryUserEnv {
    /// Starts with the raw value `raw` (`"C:\\a;C:\\b"`).
    pub fn with_path(raw: impl Into<String>) -> Self {
        Self {
            state: Mutex::new(MemoryState {
                raw: raw.into(),
                notifications: 0,
            }),
        }
    }

    /// The stored value, as it would be in the registry.
    pub fn raw_path(&self) -> String {
        self.lock().raw.clone()
    }

    /// How many times running programs would have been notified.
    pub fn notifications(&self) -> u32 {
        self.lock().notifications
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl UserEnv for MemoryUserEnv {
    fn path_entries(&self) -> Result<Vec<String>> {
        Ok(PathValue::parse(&self.lock().raw).entries())
    }

    fn add_path_entry(&self, entry: &str) -> Result<bool> {
        let mut state = self.lock();
        let mut value = PathValue::parse(&state.raw);
        let changed = value.add(entry, &|name| std::env::var(name).ok());
        if changed {
            state.raw = value.to_raw();
            state.notifications += 1;
        }
        Ok(changed)
    }

    fn remove_path_entries(&self, predicate: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        let mut state = self.lock();
        let mut value = PathValue::parse(&state.raw);
        let removed = value.remove_where(predicate);
        if !removed.is_empty() {
            state.raw = value.to_raw();
            state.notifications += 1;
        }
        Ok(removed)
    }
}

/// The user environment of the running OS.
#[cfg(windows)]
pub fn user_env() -> std::sync::Arc<dyn UserEnv> {
    std::sync::Arc::new(super::windows::WindowsUserEnv::default())
}

/// The user environment of the running OS (not implemented outside Windows yet: shells
/// read their own profile files, docs/13 §4).
#[cfg(not(windows))]
pub fn user_env() -> std::sync::Arc<dyn UserEnv> {
    std::sync::Arc::new(Unsupported)
}

#[cfg(not(windows))]
struct Unsupported;

#[cfg(not(windows))]
impl UserEnv for Unsupported {
    fn path_entries(&self) -> Result<Vec<String>> {
        Err(unsupported())
    }
    fn add_path_entry(&self, _: &str) -> Result<bool> {
        Err(unsupported())
    }
    fn remove_path_entries(&self, _: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> ForgeError {
    ForgeError::UnsupportedPlatform(format!(
        "the persistent user PATH is not managed on {}",
        std::env::consts::OS
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "USERPROFILE" => Some(r"C:\Users\dev".to_owned()),
            "ROOT" => Some("D:/Forgebox".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn expands_known_references_only() {
        assert_eq!(
            expand_env_refs(r"%USERPROFILE%\bin;%NOPE%;50%", &env),
            r"C:\Users\dev\bin;%NOPE%;50%"
        );
        assert_eq!(expand_env_refs("%%x", &env), "%%x");
        assert_eq!(expand_env_refs("%ROOT%/bin", &env), "D:/Forgebox/bin");
    }

    #[test]
    fn normalization_ignores_case_separators_quotes_and_references() {
        let n = |s: &str| normalize_path_entry(s, &env);
        assert_eq!(
            n(r"C:\Forgebox\bin\php\current\"),
            n("c:/forgebox/BIN/php/current")
        );
        assert_eq!(n(r#""C:\Forgebox\bin""#), n(r"C:\Forgebox\bin"));
        assert_eq!(n(r"%ROOT%\bin"), n(r"D:\Forgebox\bin\\"));
        assert_eq!(n(r"C:\"), r"c:\");
        assert_ne!(n(r"C:\Forgebox\bin"), n(r"C:\Forgebox\bin2"));
    }

    #[test]
    fn add_prepends_once_and_keeps_everything_else_verbatim() {
        let original = r"C:\a;;%USERPROFILE%\b;";
        let mut value = PathValue::parse(original);
        assert!(value.add(r"C:\Forgebox\bin\php\current", &env));
        assert_eq!(
            value.to_raw(),
            format!(r"C:\Forgebox\bin\php\current;{original}")
        );
        assert!(
            !value.add(r"c:/forgebox/bin/php/current/", &env),
            "already there"
        );
        assert!(
            !value.add(r"C:\Users\dev\b", &env),
            "equivalent through %USERPROFILE%"
        );
        assert_eq!(value.entries().len(), 3);

        let removed = value.remove_where(&|e| e.ends_with(r"php\current"));
        assert_eq!(removed, [r"C:\Forgebox\bin\php\current"]);
        assert_eq!(value.to_raw(), original, "add + remove restores the value");
    }

    #[test]
    fn add_collapses_existing_duplicates() {
        let mut value = PathValue::parse(r"C:\x;C:\fb\bin\shims;C:\y;c:/fb/bin/shims/");
        assert!(value.add(r"C:\fb\bin\shims", &env));
        assert_eq!(value.to_raw(), r"C:\x;C:\fb\bin\shims;C:\y");
        assert!(!value.add(r"C:\fb\bin\shims", &env));
    }

    #[test]
    fn empty_values() {
        let mut value = PathValue::parse("");
        assert!(value.entries().is_empty());
        assert!(!value.add("  ", &env));
        assert!(value.add(r"C:\fb", &env));
        assert_eq!(value.to_raw(), r"C:\fb");
        value.remove_where(&|_| true);
        assert_eq!(value.to_raw(), "");
    }

    #[test]
    fn memory_env_notifies_only_on_change() {
        let mem = MemoryUserEnv::with_path(r"C:\a");
        assert!(mem.add_path_entry(r"C:\fb\bin\shims").unwrap());
        assert!(!mem.add_path_entry(r"C:\FB\bin\shims\").unwrap());
        assert_eq!(mem.notifications(), 1);
        assert_eq!(mem.path_entries().unwrap(), [r"C:\fb\bin\shims", r"C:\a"]);
        assert!(mem.remove_path_entries(&|_| false).unwrap().is_empty());
        assert_eq!(mem.notifications(), 1);
        assert_eq!(
            mem.remove_path_entries(&|e| e.contains("fb"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(mem.raw_path(), r"C:\a");
        assert_eq!(mem.notifications(), 2);
    }
}
