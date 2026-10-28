//! `forge` command templates of the method table (ADR-0037): the mini-language that turns
//! the params of an API call into the equivalent `forge` command line, so the GUI can show
//! and copy it ("Copiar comando", docs/07 §1.7).
//!
//! The same grammar is rendered by `apps/desktop/src/ipc/cli.ts` (M2-E1-T13) from
//! `methods.ts`; both renderers must produce the strings of the shared golden file
//! `crates/forge-ipc/tests/fixtures/cli_templates.json`.
//!
//! # Grammar
//!
//! A template is one or more *alternatives* separated by ` | ` (space, pipe, space). An
//! alternative may start with a *rule* `[cond,cond]`; the first alternative whose rule
//! holds (or that has none) is rendered. Without a match there is no command (`None`).
//!
//! * rule conditions: `field=value` (the scalar's text equals `value`), `field` (truthy:
//!   not absent, `null`, `false`, `""`, `[]`, `{}`) and `!field` (not truthy);
//! * an alternative is whitespace-separated tokens. A token is either a literal word
//!   (copied as is) or a single `{...}` placeholder, rendered from the params JSON:
//!
//! | placeholder       | renders                                                                |
//! |-------------------|------------------------------------------------------------------------|
//! | `{field}`         | the value as a positional argument; nothing when absent or `null`      |
//! | `{field=--flag}`  | `--flag value`; nothing when absent or `null`                          |
//! | `{field?--flag}`  | `--flag` when the value is `true`; nothing otherwise                   |
//! | `{field?a:b}`     | `a` when `true`, `b` when `false` (either side may be empty); nothing when absent or `null` |
//! | `{list...}`       | one argument per array item; an object gives `key=value` entries sorted by key; nothing when empty |
//! | `{a.b}`           | any placeholder accepts a dotted path into nested objects              |
//!
//! Values must be strings, integers or booleans; an empty string counts as absent
//! (PowerShell 5.1 drops empty arguments of native programs). A value that cannot be
//! written so that PowerShell and `cmd.exe` pass it unchanged (see [`quote_arg`]) makes the
//! whole command unavailable (`None`) instead of a wrong one.

use std::fmt;

use serde_json::Value;

/// A template that does not follow the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid cli template: {}", self.0)
    }
}

impl std::error::Error for TemplateError {}

fn err<T>(message: impl Into<String>) -> Result<T, TemplateError> {
    Err(TemplateError(message.into()))
}

/// Path of a field inside the params (`a.b` = `params["a"]["b"]`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Path(Vec<String>);

impl Path {
    fn parse(text: &str) -> Result<Self, TemplateError> {
        let segments: Vec<&str> = text.split('.').collect();
        let valid =
            |s: &&str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if segments.iter().all(valid) {
            Ok(Self(segments.into_iter().map(str::to_owned).collect()))
        } else {
            err(format!("bad field path {text:?}"))
        }
    }

    fn get<'a>(&self, params: &'a Value) -> Option<&'a Value> {
        let mut current = params;
        for segment in &self.0 {
            current = current.as_object()?.get(segment)?;
        }
        Some(current)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Positional(Path),
    Option(Path, String),
    Flag(Path, String),
    Choice(Path, String, String),
    Variadic(Path),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Condition {
    path: Path,
    negate: bool,
    equals: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Alternative {
    conditions: Vec<Condition>,
    pieces: Vec<Piece>,
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliTemplate {
    alternatives: Vec<Alternative>,
}

fn is_word(text: &str) -> bool {
    !text.is_empty() && !text.contains(['{', '}', '[', ']'])
}

fn parse_condition(text: &str) -> Result<Condition, TemplateError> {
    let (negate, rest) = match text.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    match rest.split_once('=') {
        Some((path, value)) if !negate && is_word(value) => Ok(Condition {
            path: Path::parse(path)?,
            negate,
            equals: Some(value.to_owned()),
        }),
        Some(_) => err(format!("bad condition {text:?}")),
        None => Ok(Condition {
            path: Path::parse(rest)?,
            negate,
            equals: None,
        }),
    }
}

fn parse_placeholder(inner: &str) -> Result<Piece, TemplateError> {
    let (path_text, rest) = match inner.strip_suffix("...") {
        Some(head) => (head, "..."),
        None => {
            let end = inner
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.')))
                .unwrap_or(inner.len());
            inner.split_at(end)
        }
    };
    let path = Path::parse(path_text)?;
    if rest.is_empty() {
        return Ok(Piece::Positional(path));
    }
    if rest == "..." {
        return Ok(Piece::Variadic(path));
    }
    if let Some(flag) = rest.strip_prefix('=') {
        return if is_word(flag) && flag.starts_with('-') {
            Ok(Piece::Option(path, flag.to_owned()))
        } else {
            err(format!("bad option flag in {{{inner}}}"))
        };
    }
    if let Some(choice) = rest.strip_prefix('?') {
        return match choice.split_once(':') {
            None if is_word(choice) && choice.starts_with('-') => {
                Ok(Piece::Flag(path, choice.to_owned()))
            }
            Some((yes, no))
                if (yes.is_empty() || is_word(yes))
                    && (no.is_empty() || is_word(no))
                    && !(yes.is_empty() && no.is_empty()) =>
            {
                Ok(Piece::Choice(path, yes.to_owned(), no.to_owned()))
            }
            _ => err(format!("bad flag or choice in {{{inner}}}")),
        };
    }
    err(format!("bad placeholder {{{inner}}}"))
}

fn parse_alternative(text: &str) -> Result<Alternative, TemplateError> {
    let mut tokens = text.split_whitespace().peekable();
    let mut conditions = Vec::new();
    if let Some(first) = tokens.peek().copied()
        && let Some(rule) = first.strip_prefix('[')
    {
        let Some(rule) = rule.strip_suffix(']') else {
            return err(format!("unterminated rule in {text:?}"));
        };
        for condition in rule.split(',') {
            conditions.push(parse_condition(condition)?);
        }
        tokens.next();
    }
    let mut pieces = Vec::new();
    for token in tokens {
        if let Some(inner) = token.strip_prefix('{') {
            let Some(inner) = inner.strip_suffix('}') else {
                return err(format!("unterminated placeholder {token:?}"));
            };
            pieces.push(parse_placeholder(inner)?);
        } else if is_word(token) {
            pieces.push(Piece::Literal(token.to_owned()));
        } else {
            return err(format!("bad token {token:?}"));
        }
    }
    if !matches!(pieces.first(), Some(Piece::Literal(_))) {
        return err(format!("{text:?} must start with a literal word"));
    }
    Ok(Alternative { conditions, pieces })
}

impl CliTemplate {
    /// Parses a template; see the module docs for the grammar.
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let alternatives = source
            .split(" | ")
            .map(parse_alternative)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { alternatives })
    }

    /// Number of alternatives (rules) of the template.
    pub fn alternatives(&self) -> usize {
        self.alternatives.len()
    }

    /// Index of the alternative `params` selects, if any.
    pub fn selected(&self, params: &Value) -> Option<usize> {
        self.alternatives
            .iter()
            .position(|alt| alt.conditions.iter().all(|c| c.holds(params)))
    }

    /// Renders the command for `params`, or `None` when no alternative applies or a value
    /// cannot be quoted portably.
    pub fn render(&self, params: &Value) -> Option<String> {
        self.alternatives[self.selected(params)?].render(params)
    }
}

/// Renders `template` for `params` (parse + render); `None` for an invalid template.
pub fn render(template: &str, params: &Value) -> Option<String> {
    CliTemplate::parse(template).ok()?.render(params)
}

/// Checks the syntax of a template.
pub fn validate(template: &str) -> Result<(), TemplateError> {
    CliTemplate::parse(template).map(|_| ())
}

impl Condition {
    fn holds(&self, params: &Value) -> bool {
        let value = self.path.get(params);
        let result = match &self.equals {
            Some(expected) => value
                .and_then(|v| scalar_text(v).ok().flatten())
                .is_some_and(|text| text == *expected),
            None => value.is_some_and(truthy),
        };
        result != self.negate
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Bool(true) | Value::Number(_) => true,
    }
}

/// Text of a scalar: `Ok(None)` for absent values (`null`, empty string), `Err` for
/// values the language cannot write (arrays, objects, non-integer numbers).
fn scalar_text(value: &Value) -> Result<Option<String>, ()> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Some(n.to_string())),
        Value::Number(_) | Value::Array(_) | Value::Object(_) => Err(()),
    }
}

/// A value as one argument: its text, quoted. Values starting with `-` would be read as
/// flags, so they are unrepresentable too.
fn argument(text: &str) -> Result<String, ()> {
    if text.starts_with('-') {
        return Err(());
    }
    quote_arg(text).ok_or(())
}

impl Alternative {
    fn render(&self, params: &Value) -> Option<String> {
        let mut tokens: Vec<String> = Vec::new();
        for piece in &self.pieces {
            Self::render_piece(piece, params, &mut tokens).ok()?;
        }
        Some(tokens.join(" "))
    }

    fn render_piece(piece: &Piece, params: &Value, out: &mut Vec<String>) -> Result<(), ()> {
        match piece {
            Piece::Literal(word) => out.push(word.clone()),
            Piece::Positional(path) => {
                if let Some(text) = path.get(params).map(scalar_text).transpose()?.flatten() {
                    out.push(argument(&text)?);
                }
            }
            Piece::Option(path, flag) => {
                if let Some(text) = path.get(params).map(scalar_text).transpose()?.flatten() {
                    out.push(flag.clone());
                    out.push(argument(&text)?);
                }
            }
            Piece::Flag(path, flag) => {
                if path.get(params) == Some(&Value::Bool(true)) {
                    out.push(flag.clone());
                }
            }
            Piece::Choice(path, yes, no) => {
                let word = match path.get(params) {
                    Some(Value::Bool(true)) => yes,
                    Some(Value::Bool(false)) => no,
                    _ => return Ok(()),
                };
                if !word.is_empty() {
                    out.push(word.clone());
                }
            }
            Piece::Variadic(path) => match path.get(params) {
                None | Some(Value::Null) => {}
                Some(Value::Array(items)) => {
                    for item in items {
                        if let Some(text) = scalar_text(item)? {
                            out.push(argument(&text)?);
                        }
                    }
                }
                Some(Value::Object(entries)) => {
                    let mut keys: Vec<&String> = entries.keys().collect();
                    keys.sort();
                    for key in keys {
                        let value = scalar_text(&entries[key])?.unwrap_or_default();
                        out.push(argument(&format!("{key}={value}"))?);
                    }
                }
                Some(_) => return Err(()),
            },
        }
        Ok(())
    }
}

/// Characters that need no quoting in PowerShell or `cmd.exe`.
fn is_bare_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '=' | ':' | '.' | '/' | '\\' | '@' | '-')
}

/// Quotes one argument for PowerShell and `cmd.exe` at once, in the shape the receiving
/// program (Rust's `std::env::args`, which follows `CommandLineToArgvW`) reads back
/// unchanged:
///
/// * an argument made only of `[A-Za-z0-9_+=:./\@-]` (not starting with `@`) is written as is;
/// * anything else is wrapped in double quotes, which both shells keep literal for spaces,
///   `&`, `|`, `(`, `)`, `^`, `'`, `;`, `,` and the like; a run of backslashes at the end
///   is doubled so it does not escape the closing quote;
/// * `None` for what no common quoting survives: an empty string (PowerShell 5.1 drops
///   it), `"` (differs between the shells), `$` and `` ` `` (PowerShell expands them even
///   inside double quotes), `%` (`cmd.exe` expands `%VAR%`) and control characters.
pub fn quote_arg(value: &str) -> Option<String> {
    if value.is_empty()
        || value.chars().any(|c| {
            matches!(c, '"' | '$' | '`' | '%')
                || c <= '\u{1f}'
                || ('\u{7f}'..='\u{9f}').contains(&c)
        })
    {
        return None;
    }
    if !value.starts_with('@') && value.chars().all(is_bare_char) {
        return Some(value.to_owned());
    }
    let trailing = value.len() - value.trim_end_matches('\\').len();
    Some(format!("\"{value}{}\"", "\\".repeat(trailing)))
}

/// Splits a command line into arguments with the `CommandLineToArgvW` rules (the inverse
/// of [`quote_arg`] joined by spaces). Used by the round-trip tests.
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut backslashes = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    backslashes += 1;
                }
                started = true;
                if chars.peek() == Some(&'"') {
                    current.push_str(&"\\".repeat(backslashes / 2));
                    if backslashes % 2 == 1 {
                        chars.next();
                        current.push('"');
                    }
                } else {
                    current.push_str(&"\\".repeat(backslashes));
                }
            }
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn r(template: &str, params: Value) -> Option<String> {
        render(template, &params)
    }

    #[test]
    fn positional_is_omitted_when_null_or_absent() {
        assert_eq!(
            r("forge x {a} {b}", json!({"a": "one"})).as_deref(),
            Some("forge x one")
        );
        assert_eq!(
            r("forge x {a}", json!({"a": null})).as_deref(),
            Some("forge x")
        );
        assert_eq!(
            r("forge x {a}", json!({"a": ""})).as_deref(),
            Some("forge x")
        );
    }

    #[test]
    fn option_flag_choice_and_variadic() {
        let t = "forge x {name=--name} {on?--on} {sec?secure:unsecure} {rest...}";
        assert_eq!(
            r(
                t,
                json!({"name": "n", "on": true, "sec": false, "rest": ["a", "b c"]})
            )
            .as_deref(),
            Some("forge x --name n --on unsecure a \"b c\"")
        );
        assert_eq!(
            r(
                t,
                json!({"name": null, "on": false, "sec": null, "rest": []})
            )
            .as_deref(),
            Some("forge x")
        );
        assert_eq!(
            r("forge x {f?:--no-f}", json!({"f": false})).as_deref(),
            Some("forge x --no-f")
        );
        assert_eq!(
            r("forge x {f?:--no-f}", json!({"f": true})).as_deref(),
            Some("forge x")
        );
    }

    #[test]
    fn nested_fields_and_numbers() {
        assert_eq!(
            r("forge x {a.b=--n}", json!({"a": {"b": 7}})).as_deref(),
            Some("forge x --n 7")
        );
        assert_eq!(
            r("forge x {a.b}", json!({"a": null})).as_deref(),
            Some("forge x")
        );
    }

    #[test]
    fn objects_render_sorted_key_value_entries() {
        assert_eq!(
            r(
                "forge x {s...}",
                json!({"s": {"memory_limit": "1G", "a": "x y"}})
            )
            .as_deref(),
            Some("forge x \"a=x y\" memory_limit=1G")
        );
    }

    #[test]
    fn rules_pick_the_alternative() {
        let t = "[kind=proxy] forge proxy remove {site} | [!kind,site] forge site remove {site} | forge nothing";
        assert_eq!(
            r(t, json!({"kind": "proxy", "site": "a.test"})).as_deref(),
            Some("forge proxy remove a.test")
        );
        assert_eq!(
            r(t, json!({"site": "a.test"})).as_deref(),
            Some("forge site remove a.test")
        );
        assert_eq!(r(t, json!({})).as_deref(), Some("forge nothing"));
        assert_eq!(r("[a] forge x", json!({"a": false})), None);
    }

    #[test]
    fn unrepresentable_values_give_no_command() {
        for value in [json!("a\"b"), json!("$HOME"), json!("100%"), json!("-v")] {
            assert_eq!(r("forge x {a}", json!({ "a": value })), None);
        }
        assert_eq!(r("forge x {a}", json!({"a": {"k": 1}})), None);
        assert_eq!(r("forge x {a}", json!({"a": 1.5})), None);
    }

    #[test]
    fn invalid_templates_are_rejected() {
        for bad in [
            "",
            "{a}",
            "forge {a",
            "forge {a=b}",
            "forge {a?}",
            "forge {a?:}",
            "forge {a b}",
            "forge x{a}",
            "[a forge x",
            "[a=] forge x",
            "forge {.a}",
        ] {
            assert!(validate(bad).is_err(), "{bad:?} should be invalid");
        }
        for good in [
            "forge x",
            "forge x {a} {b=--b} {c?--c} {d?y:n} {e...} {f.g}",
            "[a=b,!c] forge x | forge y",
        ] {
            assert!(validate(good).is_ok(), "{good:?} should be valid");
        }
    }

    #[test]
    fn quoting_keeps_paths_with_spaces_and_backslashes() {
        assert_eq!(quote_arg(r"C:\www\loja").unwrap(), r"C:\www\loja");
        assert_eq!(
            quote_arg(r"C:\My Sites\loja").unwrap(),
            "\"C:\\My Sites\\loja\""
        );
        assert_eq!(quote_arg(r"C:\My Sites\").unwrap(), "\"C:\\My Sites\\\\\"");
        assert_eq!(quote_arg("php@8.3").unwrap(), "php@8.3");
        assert_eq!(quote_arg("@x").unwrap(), "\"@x\"");
        assert_eq!(quote_arg("a,b").unwrap(), "\"a,b\"");
        assert_eq!(quote_arg("a&b").unwrap(), "\"a&b\"");
        for bad in ["", "a\"b", "$x", "a`b", "50%", "a\nb"] {
            assert_eq!(quote_arg(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn quoting_round_trips_through_the_argv_rules() {
        for value in [
            r"C:\My Sites\loja",
            r"C:\My Sites\",
            r"\\server\share name\dir\",
            "plain",
            "a b  c",
            "é à",
            "x&y|z^w",
            "it's (ok); fine, yes",
            r"C:\a\\",
        ] {
            let quoted = quote_arg(value).unwrap();
            assert_eq!(
                split_command_line(&format!("forge {quoted} end")),
                ["forge", value, "end"],
                "{value:?} -> {quoted}"
            );
        }
    }
}
