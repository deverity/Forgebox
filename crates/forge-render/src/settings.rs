//! Quick settings writers (docs/04 §1.3, ADR-0042): one writer per config format that sets
//! values inside a text Forgebox owns, never anywhere else.
//!
//! * [`apply`] is the pure writer: it takes the text of a region Forgebox owns (the body of
//!   a managed block, a generated file, an overlay) and returns it with each
//!   [`Assignment`] applied, everything else byte for byte.
//!   * `ini` — target `key` or `section/key` (`mysqld/max_connections`; a dot is part of
//!     the key, `opcache.enable`). The **last** occurrence is replaced (last wins in PHP
//!     and MySQL); a missing key is added at the end of its section (the section is
//!     appended when missing); a key without section goes at the end.
//!   * `conf-directive` — target a directive name (case-insensitive, as in Apache and
//!     PostgreSQL). The last occurrence at the shallowest nesting level (`{}` of nginx,
//!     `<Section>` of Apache) gets the new arguments; separator (`name value`,
//!     `name = value`), `;` terminator, quotes and trailing comment are kept. A missing
//!     directive is appended at the top level in the style of the other lines: templates
//!     of servers with nested contexts (nginx `http {}`) must emit it where it belongs.
//!   * `yaml` — target a dotted path of block mappings (`server.port`); the scalar is
//!     replaced (comment kept), missing keys are created with the indentation in use.
//!   * `json` — target a dotted path of objects; the value is replaced in place, missing
//!     members are appended to their object (intermediate objects created).
//!   * `args` — one argument per line (the `.args` files); target a flag (`--smtp`): the
//!     line after the flag, or the part after `--flag=`, is the value; a missing flag is
//!     appended. A native boolean (toggle without `on`/`off`) is the presence of the flag.
//! * [`apply_to_rendered`] runs the writer over the files a render target produced (the
//!   managed block or generated file of a config file), so a template that renders a
//!   whole file keeps the values; [`standalone`] gives the file to write for a config
//!   file no target renders: the `overlay` (a generated file, PHP's
//!   `conf.d/90-forgebox-settings.ini`) or, without one, a managed block
//!   [`SETTINGS_BLOCK`] in the file itself.
//!
//! Pure text work except nothing: callers do the file I/O (`spawn_blocking`).

use std::ops::Range;
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};

use crate::managed::CommentStyle;
use crate::targets::{Ownership, RenderedFile};

/// Name of the managed block that holds the quick settings of a file no target renders.
pub const SETTINGS_BLOCK: &str = "settings";

/// Format of a config file with quick settings (`config.files[].format`, minus `text`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsFormat {
    Ini,
    ConfDirective,
    Yaml,
    Json,
    Args,
}

impl SettingsFormat {
    /// Comment syntax of a file Forgebox writes in this format (header, blocks).
    pub fn comment_style(self) -> CommentStyle {
        match self {
            Self::Ini => CommentStyle::Semicolon,
            Self::ConfDirective | Self::Yaml => CommentStyle::Hash,
            Self::Json | Self::Args => CommentStyle::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ini => "ini",
            Self::ConfDirective => "conf-directive",
            Self::Yaml => "yaml",
            Self::Json => "json",
            Self::Args => "args",
        }
    }
}

/// A value as the file gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    /// Text (`512M`, `On`, `/path`); quoted by the writer when the format needs it.
    Text(String),
    /// A decimal number (`120`, `0.5`): unquoted in every format.
    Number(String),
    /// The format's native boolean (a toggle without `on`/`off`): `On`/`Off` in ini,
    /// `on`/`off` in conf-directive, `true`/`false` in yaml/json, the flag's presence in
    /// args.
    Bool(bool),
    /// Items of a `multi` setting: joined by `,` (ini, args), by spaces (conf-directive),
    /// a list in yaml/json.
    List(Vec<String>),
}

/// One value to write at `target` (`mysqld/max_connections`, `client_max_body_size`,
/// `server.port`, `--smtp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub target: String,
    pub value: SettingValue,
}

impl Assignment {
    pub fn new(target: impl Into<String>, value: SettingValue) -> Self {
        Self {
            target: target.into(),
            value,
        }
    }
}

/// The quick settings of one config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFile {
    /// `config.files[].id` (`my_ini`).
    pub id: String,
    /// The config file (absolute).
    pub path: PathBuf,
    /// Where the values go instead of `path` (absolute; `config.files[].overlay`).
    pub overlay: Option<PathBuf>,
    pub format: SettingsFormat,
    pub assignments: Vec<Assignment>,
}

impl SettingsFile {
    /// The file that receives the values (the overlay when declared).
    pub fn written_path(&self) -> &Path {
        self.overlay.as_deref().unwrap_or(&self.path)
    }
}

fn invalid(target: &str, why: impl std::fmt::Display) -> ForgeError {
    ForgeError::InvalidInput(format!("{target}: {why}"))
}

/// Applies `assignments` to `text` (a region Forgebox owns) in `format`; see the module
/// docs for each format. Lines Forgebox does not touch keep their bytes.
pub fn apply(format: SettingsFormat, text: &str, assignments: &[Assignment]) -> Result<String> {
    let mut out = text.to_owned();
    for assignment in assignments {
        out = match format {
            SettingsFormat::Ini => ini(&out, assignment)?,
            SettingsFormat::ConfDirective => conf_directive(&out, assignment)?,
            SettingsFormat::Yaml => yaml(&out, assignment)?,
            SettingsFormat::Json => json(&out, assignment)?,
            SettingsFormat::Args => args(&out, assignment)?,
        };
    }
    Ok(out)
}

// ------------------------------------------------------------------------- lines

/// Lines of `text` without their `\n` (a `\r` stays on the line), and whether the text
/// ended with a newline.
fn lines_of(text: &str) -> (Vec<String>, bool) {
    if text.is_empty() {
        return (Vec::new(), true);
    }
    let trailing = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    (body.split('\n').map(str::to_owned).collect(), trailing)
}

fn join_lines(lines: &[String], trailing: bool) -> String {
    let mut out = lines.join("\n");
    if trailing && !lines.is_empty() {
        out.push('\n');
    }
    out
}

/// Splits `line` into its content and its `\r` (kept as it was).
fn split_cr(line: &str) -> (&str, &str) {
    match line.strip_suffix('\r') {
        Some(content) => (content, "\r"),
        None => (line, ""),
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Rejects line breaks and other control characters (a value is always one line).
fn one_line(target: &str, text: &str) -> Result<()> {
    if text.chars().any(|c| c.is_control()) {
        return Err(invalid(
            target,
            "values cannot hold line breaks or control characters",
        ));
    }
    Ok(())
}

fn joined(target: &str, items: &[String], sep: &str) -> Result<String> {
    for item in items {
        one_line(target, item)?;
    }
    Ok(items.join(sep))
}

// --------------------------------------------------------------------------- ini

fn ini_value(target: &str, value: &SettingValue) -> Result<String> {
    let text = match value {
        SettingValue::Number(n) => return Ok(n.clone()),
        SettingValue::Bool(b) => return Ok(if *b { "On" } else { "Off" }.to_owned()),
        SettingValue::Text(t) => t.clone(),
        SettingValue::List(items) => joined(target, items, ",")?,
    };
    one_line(target, &text)?;
    let plain = !text.is_empty()
        && text.trim() == text
        && !text.contains([';', '#', '"', '='])
        && !text.starts_with('\'');
    if plain {
        Ok(text)
    } else if text.contains('"') {
        Err(invalid(
            target,
            "an ini value cannot hold `\"` together with `;`, `#` or `=`",
        ))
    } else {
        Ok(format!("\"{text}\""))
    }
}

fn ini_header(line: &str) -> Option<&str> {
    let t = line.trim();
    t.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

/// The key of an option line (`key = value`, `key=value`, a bare `flag`).
fn ini_key(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with([';', '#', '[']) {
        return None;
    }
    Some(t.split_once('=').map_or(t, |(k, _)| k).trim())
}

fn ini_replace(line: &str, key: &str, value: &str) -> String {
    let (content, cr) = split_cr(line);
    let Some(eq) = content.find('=') else {
        let indent = &content[..indent_of(content)];
        return format!("{indent}{key} = {value}{cr}");
    };
    let after = &content[eq + 1..];
    let lead = after.len() - after.trim_start().len();
    let prefix = &content[..eq + 1 + lead];
    let rest = &after[lead..];
    // A trailing comment (` ; ...` / ` # ...`) after an unquoted value stays.
    let comment = if rest.starts_with(['"', '\'']) {
        ""
    } else {
        rest.char_indices()
            .find(|&(i, c)| {
                matches!(c, ';' | '#') && i > 0 && rest[..i].ends_with(char::is_whitespace)
            })
            .map(|(i, _)| {
                let start = rest[..i].trim_end().len();
                &rest[start..]
            })
            .unwrap_or("")
    };
    format!("{prefix}{value}{comment}{cr}")
}

fn ini(text: &str, a: &Assignment) -> Result<String> {
    let (section, key) = match a.target.split_once('/') {
        Some((section, key)) => (Some(section.trim()), key.trim()),
        None => (None, a.target.trim()),
    };
    if key.is_empty() || section.is_some_and(str::is_empty) {
        return Err(invalid(&a.target, "is `key` or `section/key`"));
    }
    let value = ini_value(&a.target, &a.value)?;
    let (mut lines, trailing) = lines_of(text);
    let mut current: Option<String> = None;
    let mut found = None;
    // Last content line of the wanted section (where a missing key goes).
    let mut section_end = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(name) = ini_header(line) {
            current = Some(name.to_owned());
            if section == Some(name) {
                section_end = Some(i);
            }
            continue;
        }
        let in_section = match section {
            Some(s) => current.as_deref() == Some(s),
            None => true,
        };
        if !in_section {
            continue;
        }
        if section.is_some() && !line.trim().is_empty() {
            section_end = Some(i);
        }
        if ini_key(line) == Some(key) {
            found = Some(i);
        }
    }
    let new_line = format!("{key} = {value}");
    match (found, section, section_end) {
        (Some(i), _, _) => lines[i] = ini_replace(&lines[i], key, &value),
        (None, Some(_), Some(end)) => lines.insert(end + 1, new_line),
        (None, Some(section), None) => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(new_line);
        }
        (None, None, _) => lines.push(new_line),
    }
    Ok(join_lines(&lines, trailing))
}

// ---------------------------------------------------------------- conf-directive

fn directive_word(target: &str, word: &str) -> Result<String> {
    one_line(target, word)?;
    let plain = !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ';' | '#' | '{' | '}' | '"' | '\''));
    if plain {
        Ok(word.to_owned())
    } else if word.contains('"') {
        Err(invalid(target, "a directive argument cannot hold `\"`"))
    } else {
        Ok(format!("\"{word}\""))
    }
}

fn directive_value(target: &str, value: &SettingValue) -> Result<String> {
    match value {
        SettingValue::Number(n) => Ok(n.clone()),
        SettingValue::Bool(b) => Ok(if *b { "on" } else { "off" }.to_owned()),
        SettingValue::Text(t) => directive_word(target, t),
        SettingValue::List(items) => Ok(items
            .iter()
            .map(|i| directive_word(target, i))
            .collect::<Result<Vec<_>>>()?
            .join(" ")),
    }
}

/// A directive line: `<indent>name<sep>value<term><comment>`.
struct Directive {
    name: String,
    /// Byte range of the value inside the line's content.
    value: Range<usize>,
    sep_equals: bool,
    term: bool,
}

/// Index of a `#` comment outside quotes, preceded by whitespace (or at the start).
fn comment_start(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev_space => return Some(i),
            None => {}
        }
        prev_space = c.is_whitespace();
    }
    None
}

fn parse_directive(content: &str) -> Option<Directive> {
    let start = indent_of(content);
    let t = &content[start..];
    if t.is_empty() || t.starts_with(['#', '<', '{', '}', ';']) {
        return None;
    }
    let name_len = t
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .unwrap_or(t.len());
    if name_len == 0 {
        return None;
    }
    let name = &t[..name_len];
    let rest = &t[name_len..];
    let ws = rest.len() - rest.trim_start().len();
    let after_ws = &rest[ws..];
    let (sep_equals, sep_len) = if let Some(eq) = after_ws.strip_prefix('=') {
        (true, ws + 1 + (eq.len() - eq.trim_start().len()))
    } else if ws > 0 || after_ws.is_empty() || after_ws.starts_with(';') {
        (false, ws)
    } else {
        return None;
    };
    let value_start = start + name_len + sep_len;
    let tail = &content[value_start..];
    let end = comment_start(tail).unwrap_or(tail.len());
    let body = tail[..end].trim_end();
    if body.ends_with('{') {
        return None;
    }
    let (body, term) = match body.strip_suffix(';') {
        Some(b) => (b.trim_end(), true),
        None => (body, false),
    };
    Some(Directive {
        name: name.to_owned(),
        value: value_start..value_start + body.len(),
        sep_equals,
        term,
    })
}

/// Nesting depth before each line (`{`/`}` blocks, `<Section>`/`</Section>`).
fn depths(lines: &[String]) -> Vec<usize> {
    let mut depth = 0usize;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let (content, _) = split_cr(line);
        let t = content.trim();
        let code = &t[..comment_start(t).unwrap_or(t.len())].trim_end();
        if code.starts_with('}') || code.starts_with("</") {
            depth = depth.saturating_sub(1);
        }
        out.push(depth);
        if code.ends_with('{') || (code.starts_with('<') && !code.starts_with("</")) {
            depth += 1;
        }
    }
    out
}

fn conf_directive(text: &str, a: &Assignment) -> Result<String> {
    let name = a.target.trim();
    let value = directive_value(&a.target, &a.value)?;
    let (mut lines, trailing) = lines_of(text);
    let levels = depths(&lines);
    let mut best: Option<(usize, usize)> = None; // (depth, line)
    for (i, line) in lines.iter().enumerate() {
        let (content, _) = split_cr(line);
        let Some(d) = parse_directive(content) else {
            continue;
        };
        if !d.name.eq_ignore_ascii_case(name) {
            continue;
        }
        let depth = levels[i];
        if best.is_none_or(|(bd, _)| depth <= bd) {
            best = Some((depth, i));
        }
    }
    if let Some((_, i)) = best {
        let (content, cr) = split_cr(&lines[i]);
        let d = parse_directive(content).expect("parsed above");
        let old = &content[d.value.clone()];
        let quote = old
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'') && old.len() >= 2 && old.ends_with(*c));
        let new_value = match (quote, &a.value) {
            (Some(q), SettingValue::Text(t)) => {
                if t.contains(q) {
                    return Err(invalid(&a.target, format!("the value cannot hold `{q}`")));
                }
                one_line(&a.target, t)?;
                format!("{q}{t}{q}")
            }
            _ => value,
        };
        lines[i] = format!(
            "{}{}{}{cr}",
            &content[..d.value.start],
            new_value,
            &content[d.value.end..]
        );
    } else {
        // Same style as the first top-level directive of the text.
        let style = lines
            .iter()
            .zip(&levels)
            .filter(|(_, depth)| **depth == 0)
            .find_map(|(line, _)| parse_directive(split_cr(line).0));
        let (sep, term) = match style {
            Some(d) => (
                if d.sep_equals { " = " } else { " " },
                if d.term { ";" } else { "" },
            ),
            None => (" ", ""),
        };
        lines.push(format!("{name}{sep}{value}{term}"));
    }
    Ok(join_lines(&lines, trailing))
}

// -------------------------------------------------------------------------- yaml

fn yaml_scalar(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let reserved = matches!(
        lower.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~" | "y" | "n"
    );
    let numeric = text.parse::<f64>().is_ok() || text.starts_with(['0', '+', '.']);
    let plain = !text.is_empty()
        && !reserved
        && !numeric
        && text.trim() == text
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '.' | '/' | '-' | '@' | '%' | '+' | ' ' | ':')
        })
        // `:` only inside a word (`C:/x`), never as `key: value` or at the end.
        && !text.contains(": ")
        && !text.ends_with(':')
        && !text.starts_with(['-', '@', '%', ':']);
    if plain {
        text.to_owned()
    } else {
        serde_json::to_string(text).unwrap_or_default()
    }
}

fn yaml_value(target: &str, value: &SettingValue) -> Result<String> {
    match value {
        SettingValue::Number(n) => Ok(n.clone()),
        SettingValue::Bool(b) => Ok(b.to_string()),
        SettingValue::Text(t) => {
            one_line(target, t)?;
            Ok(yaml_scalar(t))
        }
        SettingValue::List(items) => {
            for item in items {
                one_line(target, item)?;
            }
            let items: Vec<String> = items
                .iter()
                .map(|i| serde_json::to_string(i).unwrap_or_default())
                .collect();
            Ok(format!("[{}]", items.join(", ")))
        }
    }
}

fn yaml_content(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#') && t != "---"
}

/// `(key, offset of the value part)` of a mapping line `key: value`.
fn yaml_key(line: &str) -> Option<(String, usize)> {
    let start = indent_of(line);
    let t = &line[start..];
    let (key, rest_at) = if let Some(q) = t.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        let close = t[1..].find(q)? + 1;
        (t[1..close].to_owned(), close + 1)
    } else {
        let colon = t.find(':')?;
        (t[..colon].trim_end().to_owned(), colon)
    };
    let rest = &t[rest_at..];
    let after = rest.strip_prefix(':')?;
    if !(after.is_empty() || after.starts_with([' ', '\t', '\r'])) {
        return None;
    }
    Some((key, start + rest_at + 1))
}

fn yaml(text: &str, a: &Assignment) -> Result<String> {
    let segments: Vec<&str> = a.target.split('.').map(str::trim).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(invalid(&a.target, "has an empty segment"));
    }
    let value = yaml_value(&a.target, &a.value)?;
    let (mut lines, trailing) = lines_of(text);
    let mut range = 0..lines.len();
    let mut parent_indent: Option<usize> = None;
    for (depth, segment) in segments.iter().enumerate() {
        let last = depth + 1 == segments.len();
        let child_indent = lines[range.clone()]
            .iter()
            .find(|l| yaml_content(l))
            .map(|l| indent_of(l))
            .filter(|i| parent_indent.is_none_or(|p| *i > p));
        let found = child_indent.and_then(|indent| {
            range.clone().find(|&i| {
                indent_of(&lines[i]) == indent
                    && yaml_key(split_cr(&lines[i]).0).is_some_and(|(k, _)| k == *segment)
            })
        });
        if let Some(i) = found.filter(|_| {
            lines[range.clone()]
                .iter()
                .find(|l| yaml_content(l))
                .is_none_or(|l| !l.trim_start().starts_with("- "))
        }) {
            let indent = indent_of(&lines[i]);
            // The block of this key: the lines after it that are deeper (or blank).
            let mut end = i + 1;
            while end < range.end && (!yaml_content(&lines[end]) || indent_of(&lines[end]) > indent)
            {
                end += 1;
            }
            if last {
                let (content, cr) = split_cr(&lines[i]);
                let (_, at) = yaml_key(content).expect("matched above");
                let rest = &content[at..];
                let current = rest.trim();
                let has_children = lines[i + 1..end].iter().any(|l| yaml_content(l));
                if (current.is_empty() || current.starts_with('#')) && has_children {
                    return Err(ForgeError::ConfigInvalid(format!(
                        "{}: is a mapping or a list, not a value",
                        a.target
                    )));
                }
                let comment = comment_start(rest.trim_start())
                    .map(|c| {
                        let r = rest.trim_start();
                        format!(" {}", &r[c..])
                    })
                    .unwrap_or_default();
                lines[i] = format!("{}{} {value}{comment}{cr}", &content[..at - 1], ":");
                return Ok(join_lines(&lines, trailing));
            }
            parent_indent = Some(indent);
            range = i + 1..end;
            continue;
        }
        // Missing from here on: create the rest of the path at the end of this block.
        let indent = child_indent.unwrap_or_else(|| parent_indent.map_or(0, |p| p + 2));
        let mut insert_at = range.end;
        while insert_at > range.start && !yaml_content(&lines[insert_at - 1]) {
            insert_at -= 1;
        }
        let mut new_lines = Vec::new();
        for (k, seg) in segments[depth..].iter().enumerate() {
            let pad = " ".repeat(indent + 2 * k);
            let key = yaml_scalar(seg);
            if depth + k + 1 == segments.len() {
                new_lines.push(format!("{pad}{key}: {value}"));
            } else {
                new_lines.push(format!("{pad}{key}:"));
            }
        }
        for (k, line) in new_lines.into_iter().enumerate() {
            lines.insert(insert_at + k, line);
        }
        return Ok(join_lines(&lines, trailing));
    }
    unreachable!("the loop returns on the last segment")
}

// -------------------------------------------------------------------------- json

/// A JSON value found in the text: its span and, for objects, the members.
struct JsonSpan {
    span: Range<usize>,
    /// `(key, value)` of an object, in order; `None` for other values.
    members: Option<Vec<(String, JsonSpan)>>,
}

struct JsonScanner<'a> {
    text: &'a str,
    pos: usize,
}

impl JsonScanner<'_> {
    fn error(&self, what: &str) -> ForgeError {
        ForgeError::ConfigInvalid(format!("invalid JSON at byte {}: {what}", self.pos))
    }

    fn ws(&mut self) {
        let rest = &self.text[self.pos..];
        self.pos += rest.len() - rest.trim_start().len();
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn expect(&mut self, b: u8) -> Result<()> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected `{}`", b as char)))
        }
    }

    fn string(&mut self) -> Result<String> {
        let start = self.pos;
        self.expect(b'"')?;
        let bytes = self.text.as_bytes();
        while let Some(&b) = bytes.get(self.pos) {
            self.pos += 1;
            match b {
                b'\\' => self.pos += 1,
                b'"' => {
                    return serde_json::from_str(&self.text[start..self.pos])
                        .map_err(|_| self.error("bad string"));
                }
                _ => {}
            }
        }
        Err(self.error("unterminated string"))
    }

    fn value(&mut self, depth: usize) -> Result<JsonSpan> {
        if depth > 64 {
            return Err(self.error("nested too deeply"));
        }
        self.ws();
        let start = self.pos;
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                let mut members = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                } else {
                    loop {
                        self.ws();
                        let key = self.string()?;
                        self.ws();
                        self.expect(b':')?;
                        let value = self.value(depth + 1)?;
                        members.push((key, value));
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.pos += 1,
                            Some(b'}') => {
                                self.pos += 1;
                                break;
                            }
                            _ => return Err(self.error("expected `,` or `}`")),
                        }
                    }
                }
                Ok(JsonSpan {
                    span: start..self.pos,
                    members: Some(members),
                })
            }
            Some(b'[') => {
                self.pos += 1;
                self.ws();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                } else {
                    loop {
                        self.value(depth + 1)?;
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.pos += 1,
                            Some(b']') => {
                                self.pos += 1;
                                break;
                            }
                            _ => return Err(self.error("expected `,` or `]`")),
                        }
                    }
                }
                Ok(JsonSpan {
                    span: start..self.pos,
                    members: None,
                })
            }
            Some(b'"') => {
                self.string()?;
                Ok(JsonSpan {
                    span: start..self.pos,
                    members: None,
                })
            }
            Some(_) => {
                let rest = &self.text[self.pos..];
                let len = rest
                    .find(|c: char| matches!(c, ',' | '}' | ']') || c.is_whitespace())
                    .unwrap_or(rest.len());
                let token = &rest[..len];
                if serde_json::from_str::<serde_json::Value>(token).is_err() {
                    return Err(self.error("bad literal"));
                }
                self.pos += len;
                Ok(JsonSpan {
                    span: start..self.pos,
                    members: None,
                })
            }
            None => Err(self.error("unexpected end")),
        }
    }
}

fn json_value(target: &str, value: &SettingValue) -> Result<String> {
    let v = match value {
        SettingValue::Number(n) => match serde_json::from_str::<serde_json::Number>(n) {
            Ok(n) => serde_json::Value::Number(n),
            Err(_) => serde_json::Value::String(n.clone()),
        },
        SettingValue::Bool(b) => serde_json::Value::Bool(*b),
        SettingValue::Text(t) => {
            one_line(target, t)?;
            serde_json::Value::String(t.clone())
        }
        SettingValue::List(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|i| serde_json::Value::String(i.clone()))
                .collect(),
        ),
    };
    serde_json::to_string(&v).map_err(|e| invalid(target, e))
}

fn json(text: &str, a: &Assignment) -> Result<String> {
    let segments: Vec<&str> = a.target.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(invalid(&a.target, "has an empty segment"));
    }
    let value = json_value(&a.target, &a.value)?;
    let source = if text.trim().is_empty() {
        "{}\n".to_owned()
    } else {
        text.to_owned()
    };
    let mut scanner = JsonScanner {
        text: &source,
        pos: 0,
    };
    let root = scanner.value(0)?;
    scanner.ws();
    if scanner.pos != source.len() {
        return Err(scanner.error("text after the value"));
    }
    let mut node = &root;
    for (depth, segment) in segments.iter().enumerate() {
        let Some(members) = &node.members else {
            return Err(ForgeError::ConfigInvalid(format!(
                "{}: `{}` is not an object",
                a.target,
                segments[..depth].join(".")
            )));
        };
        if let Some((_, child)) = members.iter().rev().find(|(k, _)| k == segment) {
            if depth + 1 == segments.len() {
                let mut out = source.clone();
                out.replace_range(child.span.clone(), &value);
                return Ok(out);
            }
            node = child;
            continue;
        }
        // Missing: `"seg": {"next": ... value}` appended to this object.
        let mut nested = value.clone();
        for seg in segments[depth + 1..].iter().rev() {
            nested = format!(
                "{{{}: {nested}}}",
                serde_json::to_string(seg).unwrap_or_default()
            );
        }
        let member = format!(
            "{}: {nested}",
            serde_json::to_string(segment).unwrap_or_default()
        );
        let mut out = source.clone();
        match members.last() {
            Some((_, last)) => {
                // Same separator as between the object's opening and its first member.
                let first_key_at = source[node.span.start + 1..]
                    .find('"')
                    .map_or(node.span.start + 1, |i| node.span.start + 1 + i);
                let lead = &source[node.span.start + 1..first_key_at];
                let sep = if lead.contains('\n') {
                    lead.to_owned()
                } else {
                    " ".to_owned()
                };
                out.insert_str(last.span.end, &format!(",{sep}{member}"));
            }
            None => {
                let inner = node.span.start + 1..node.span.end - 1;
                out.replace_range(inner, &member);
            }
        }
        return Ok(out);
    }
    unreachable!("the loop returns on the last segment")
}

// -------------------------------------------------------------------------- args

fn args(text: &str, a: &Assignment) -> Result<String> {
    let flag = a.target.trim();
    let (mut lines, trailing) = lines_of(text);
    let is_flag = |l: &String| split_cr(l).0.trim() == flag;
    let prefix = format!("{flag}=");
    let value = match &a.value {
        SettingValue::Bool(on) => {
            let present = lines.iter().any(is_flag);
            if *on && !present {
                lines.push(flag.to_owned());
            } else if !on {
                lines.retain(|l| !is_flag(l) && !split_cr(l).0.trim().starts_with(&prefix));
            }
            return Ok(join_lines(&lines, trailing || text.is_empty()));
        }
        SettingValue::Number(n) => n.clone(),
        SettingValue::Text(t) => t.clone(),
        SettingValue::List(items) => joined(&a.target, items, ",")?,
    };
    one_line(&a.target, &value)?;
    if let Some(i) = lines.iter().rposition(is_flag) {
        let has_value = lines
            .get(i + 1)
            .is_some_and(|l| !split_cr(l).0.trim_start().starts_with('-'));
        if has_value {
            lines[i + 1] = value;
        } else {
            lines.insert(i + 1, value);
        }
    } else if let Some(i) = lines
        .iter()
        .rposition(|l| split_cr(l).0.trim().starts_with(&prefix))
    {
        lines[i] = format!("{prefix}{value}");
    } else {
        lines.push(flag.to_owned());
        lines.push(value);
    }
    Ok(join_lines(&lines, trailing || text.is_empty()))
}

// ------------------------------------------------------------------------- files

fn relative_to_etc(etc: &Path, path: &Path) -> Result<String> {
    let etc_text = forge_core::paths::posix_string(etc);
    let path_text = forge_core::paths::posix_string(path);
    let lower_etc = etc_text.to_ascii_lowercase();
    let lower = path_text.to_ascii_lowercase();
    lower
        .strip_prefix(&format!("{}/", lower_etc.trim_end_matches('/')))
        .map(|rest| path_text[path_text.len() - rest.len()..].to_owned())
        .filter(|rest| !rest.split('/').any(|p| p == ".." || p.is_empty()))
        .ok_or_else(|| {
            ForgeError::ConfigInvalid(format!(
                "{path_text}: quick settings are only written under {etc_text}"
            ))
        })
}

fn same_file(a: &Path, b: &Path) -> bool {
    forge_core::paths::posix_string(a).eq_ignore_ascii_case(&forge_core::paths::posix_string(b))
}

/// Runs the writers over the files a target rendered: a [`SettingsFile`] without overlay
/// whose `path` is one of `files` (relative to `etc`) gets its assignments applied to that
/// file's body (the managed block, or the generated file). Returns the indexes of the
/// `settings` no rendered file took (write them with [`standalone`]).
pub fn apply_to_rendered(
    files: &mut [RenderedFile],
    etc: &Path,
    settings: &[SettingsFile],
) -> Result<Vec<usize>> {
    let mut rest = Vec::new();
    for (i, plan) in settings.iter().enumerate() {
        let target = plan.written_path();
        match files
            .iter_mut()
            .find(|f| same_file(&etc.join(&f.path), target))
        {
            Some(file) => file.body = apply(plan.format, &file.body, &plan.assignments)?,
            None => rest.push(i),
        }
    }
    Ok(rest)
}

/// The file to write for `plan` when no target renders it: the overlay as a generated
/// file (header + values), or a [`SETTINGS_BLOCK`] managed block inside the config file
/// (the rest of the file untouched). Formats without comments (json, args) need an
/// overlay or a target: `config_invalid`.
pub fn standalone(plan: &SettingsFile, etc: &Path) -> Result<RenderedFile> {
    let style = plan.format.comment_style();
    let body = apply(plan.format, "", &plan.assignments)?;
    match &plan.overlay {
        Some(overlay) => Ok(RenderedFile {
            path: relative_to_etc(etc, overlay)?,
            body,
            ownership: Ownership::Generated,
            style,
        }),
        None => {
            if style.prefix().is_none() {
                return Err(ForgeError::ConfigInvalid(format!(
                    "{}: a {} file has no comments for a managed block; declare \
                     `config.files[].overlay` (or a render target) for its quick settings",
                    plan.path.display(),
                    plan.format.as_str()
                )));
            }
            Ok(RenderedFile {
                path: relative_to_etc(etc, &plan.path)?,
                body,
                ownership: Ownership::Managed(SETTINGS_BLOCK),
                style,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn t(target: &str, v: &str) -> Assignment {
        Assignment::new(target, SettingValue::Text(v.into()))
    }

    fn n(target: &str, v: &str) -> Assignment {
        Assignment::new(target, SettingValue::Number(v.into()))
    }

    // ---- ini

    #[test]
    fn ini_replaces_the_last_occurrence_in_its_section() {
        let text = "[client]\nport = 3306\n\n[mysqld]\nport = 3306\nmax_connections = 200 # dev\nmax_connections=151\n\n[mysqldump]\nquick\nmax_allowed_packet = 256M\n";
        let out = apply(
            SettingsFormat::Ini,
            text,
            &[
                n("mysqld/max_connections", "300"),
                t("mysqldump/max_allowed_packet", "1G"),
                n("mysqld/port", "3307"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "[client]\nport = 3306\n\n[mysqld]\nport = 3307\nmax_connections = 200 # dev\nmax_connections=300\n\n[mysqldump]\nquick\nmax_allowed_packet = 1G\n"
        );
    }

    #[test]
    fn ini_adds_missing_keys_to_their_section_or_a_new_one() {
        let text = "[mysqld]\nport = 1\n\n[mysqldump]\nquick\n";
        let out = apply(
            SettingsFormat::Ini,
            text,
            &[
                n("mysqld/innodb_flush_log_at_trx_commit", "0"),
                t("client/host", "127.0.0.1"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "[mysqld]\nport = 1\ninnodb_flush_log_at_trx_commit = 0\n\n[mysqldump]\nquick\n\n[client]\nhost = 127.0.0.1\n"
        );
    }

    #[test]
    fn ini_flat_keys_keep_dots_and_comments() {
        let text = "[PHP]\nmemory_limit = 128M ; low\n[opcache]\nopcache.enable = 1\n";
        let out = apply(
            SettingsFormat::Ini,
            text,
            &[
                t("memory_limit", "1G"),
                t("opcache.enable", "0"),
                Assignment::new("display_errors", SettingValue::Bool(false)),
                Assignment::new(
                    "xdebug.mode",
                    SettingValue::List(vec!["debug".into(), "develop".into()]),
                ),
                t("error_log", "C:/fb/logs/php error.log"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "[PHP]\nmemory_limit = 1G ; low\n[opcache]\nopcache.enable = 0\ndisplay_errors = Off\nxdebug.mode = debug,develop\nerror_log = C:/fb/logs/php error.log\n"
        );
        let quoted = apply(SettingsFormat::Ini, "", &[t("a", "x;y")]).unwrap();
        assert_eq!(quoted, "a = \"x;y\"\n");
        assert!(apply(SettingsFormat::Ini, "", &[t("a", "x\ny")]).is_err());
        assert!(apply(SettingsFormat::Ini, "", &[t("a", "\"x;\"")]).is_err());
        assert!(apply(SettingsFormat::Ini, "", &[t("/a", "x")]).is_err());
    }

    #[test]
    fn ini_overlay_from_nothing() {
        let out = apply(
            SettingsFormat::Ini,
            "",
            &[t("memory_limit", "1G"), t("post_max_size", "256M")],
        )
        .unwrap();
        assert_eq!(out, "memory_limit = 1G\npost_max_size = 256M\n");
    }

    #[test]
    fn crlf_lines_keep_their_carriage_return() {
        let out = apply(SettingsFormat::Ini, "a = 1\r\nb = 2\r\n", &[n("a", "3")]).unwrap();
        assert_eq!(out, "a = 3\r\nb = 2\r\n");
    }

    // ---- conf-directive

    #[test]
    fn nginx_directives_keep_their_place_and_terminator() {
        let text = "worker_processes 1;\nerror_log \"x\";\n\nhttp {\n    keepalive_timeout 65;\n    client_max_body_size 128M; # quick setting\n    server {\n        client_max_body_size 256M;\n    }\n}\n";
        let out = apply(
            SettingsFormat::ConfDirective,
            text,
            &[
                t("worker_processes", "2"),
                n("keepalive_timeout", "30"),
                t("client_max_body_size", "1G"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "worker_processes 2;\nerror_log \"x\";\n\nhttp {\n    keepalive_timeout 30;\n    client_max_body_size 1G; # quick setting\n    server {\n        client_max_body_size 256M;\n    }\n}\n"
        );
    }

    #[test]
    fn apache_and_postgresql_styles() {
        let apache = "ServerRoot \"C:/x\"\nTimeout 300\n<VirtualHost *:80>\n    Timeout 10\n</VirtualHost>\n";
        let out = apply(
            SettingsFormat::ConfDirective,
            apache,
            &[
                n("timeout", "60"),
                Assignment::new("KeepAlive", SettingValue::Bool(true)),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "ServerRoot \"C:/x\"\nTimeout 60\n<VirtualHost *:80>\n    Timeout 10\n</VirtualHost>\nKeepAlive on\n"
        );
        let pg = "data_directory = 'C:/d'\nmax_connections = 100\nshared_buffers = 128MB\n";
        let out = apply(
            SettingsFormat::ConfDirective,
            pg,
            &[
                n("max_connections", "300"),
                t("data_directory", "D:/data"),
                t("synchronous_commit", "on"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "data_directory = 'D:/data'\nmax_connections = 300\nshared_buffers = 128MB\nsynchronous_commit = on\n"
        );
        let out = apply(SettingsFormat::ConfDirective, "", &[t("a", "two words")]).unwrap();
        assert_eq!(out, "a \"two words\"\n");
        assert!(
            apply(
                SettingsFormat::ConfDirective,
                pg,
                &[t("data_directory", "it's")]
            )
            .is_err()
        );
    }

    // ---- yaml

    #[test]
    fn yaml_replaces_and_creates_paths() {
        let text =
            "# config\nserver:\n  port: 8080 # http\n  host: 127.0.0.1\nlog:\n  level: info\n";
        let out = apply(
            SettingsFormat::Yaml,
            text,
            &[
                n("server.port", "9090"),
                Assignment::new("server.tls", SettingValue::Bool(false)),
                t("log.file", "C:/fb/logs/x.log"),
                t("cache.dir", "yes"),
                Assignment::new(
                    "modes",
                    SettingValue::List(vec!["debug".into(), "a,b".into()]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "# config\nserver:\n  port: 9090 # http\n  host: 127.0.0.1\n  tls: false\nlog:\n  level: info\n  file: C:/fb/logs/x.log\ncache:\n  dir: \"yes\"\nmodes: [\"debug\", \"a,b\"]\n"
        );
        let err = apply(SettingsFormat::Yaml, text, &[n("server", "1")]).unwrap_err();
        assert_eq!(err.code(), "config_invalid");
        assert_eq!(
            apply(SettingsFormat::Yaml, "", &[t("a.b", "c")]).unwrap(),
            "a:\n  b: c\n"
        );
    }

    // ---- json

    #[test]
    fn json_replaces_values_in_place_and_appends_members() {
        let text = "{\n  \"server\": {\n    \"port\": 8080,\n    \"host\": \"127.0.0.1\"\n  },\n  \"debug\": false\n}\n";
        let out = apply(
            SettingsFormat::Json,
            text,
            &[
                n("server.port", "9090"),
                Assignment::new("debug", SettingValue::Bool(true)),
                t("server.name", "fb"),
                t("log.level", "info"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "{\n  \"server\": {\n    \"port\": 9090,\n    \"host\": \"127.0.0.1\",\n    \"name\": \"fb\"\n  },\n  \"debug\": true,\n  \"log\": {\"level\": \"info\"}\n}\n"
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["server"]["port"], 9090);
        assert_eq!(
            apply(SettingsFormat::Json, "", &[t("a", "b")]).unwrap(),
            "{\"a\": \"b\"}\n"
        );
        assert_eq!(
            apply(SettingsFormat::Json, "{}", &[n("a.b", "1.5")]).unwrap(),
            "{\"a\": {\"b\": 1.5}}"
        );
        let err = apply(SettingsFormat::Json, "{\"a\": 1}", &[n("a.b", "1")]).unwrap_err();
        assert_eq!(err.code(), "config_invalid");
        assert!(apply(SettingsFormat::Json, "{\"a\": }", &[n("a", "1")]).is_err());
    }

    // ---- args

    #[test]
    fn args_set_values_and_flags() {
        let text = "--listen\n127.0.0.1:8025\n--smtp=127.0.0.1:1025\n--verbose\n";
        let out = apply(
            SettingsFormat::Args,
            text,
            &[
                t("--listen", "127.0.0.1:9000"),
                t("--smtp", "127.0.0.1:2525"),
                Assignment::new("--verbose", SettingValue::Bool(false)),
                Assignment::new("--quiet", SettingValue::Bool(true)),
                n("--max", "10"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "--listen\n127.0.0.1:9000\n--smtp=127.0.0.1:2525\n--quiet\n--max\n10\n"
        );
    }

    // ---- files

    fn plan(path: &str, overlay: Option<&str>, format: SettingsFormat) -> SettingsFile {
        SettingsFile {
            id: "f".into(),
            path: PathBuf::from(path),
            overlay: overlay.map(PathBuf::from),
            format,
            assignments: vec![t("memory_limit", "1G")],
        }
    }

    #[test]
    fn rendered_files_take_the_values_and_the_rest_is_standalone() {
        let etc = Path::new("C:/fb/etc");
        let mut files = vec![RenderedFile {
            path: "mysql/default/my.ini".into(),
            body: "[mysqld]\nmemory_limit = 1M\n".into(),
            ownership: Ownership::Managed("main"),
            style: CommentStyle::Hash,
        }];
        let plans = vec![
            plan("C:/FB/etc/mysql/default/my.ini", None, SettingsFormat::Ini),
            plan(
                "C:/fb/etc/php/8.4/php.ini",
                Some("C:/fb/etc/php/8.4/conf.d/90-forgebox-settings.ini"),
                SettingsFormat::Ini,
            ),
        ];
        let rest = apply_to_rendered(&mut files, etc, &plans).unwrap();
        assert_eq!(files[0].body, "[mysqld]\nmemory_limit = 1G\n");
        assert_eq!(rest, [1]);
        let overlay = standalone(&plans[1], etc).unwrap();
        assert_eq!(overlay.path, "php/8.4/conf.d/90-forgebox-settings.ini");
        assert_eq!(overlay.ownership, Ownership::Generated);
        assert_eq!(overlay.style, CommentStyle::Semicolon);
        assert_eq!(overlay.body, "memory_limit = 1G\n");
        let block = standalone(&plan("C:/fb/etc/x/x.ini", None, SettingsFormat::Ini), etc).unwrap();
        assert_eq!(block.ownership, Ownership::Managed(SETTINGS_BLOCK));
        assert_eq!(block.path, "x/x.ini");
        let json = standalone(&plan("C:/fb/etc/x.json", None, SettingsFormat::Json), etc);
        assert_eq!(json.unwrap_err().code(), "config_invalid");
        let outside = standalone(&plan("C:/elsewhere/x.ini", None, SettingsFormat::Ini), etc);
        assert_eq!(outside.unwrap_err().code(), "config_invalid");
        let escape = standalone(&plan("C:/fb/etc/../x.ini", None, SettingsFormat::Ini), etc);
        assert!(escape.is_err());
    }
}
