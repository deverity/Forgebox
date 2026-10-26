//! The minijinja environment (docs/03 §3).
//!
//! Templates are embedded with `include_dir!` from `crates/forge-render/templates/`.
//! A file at `<root>/etc/templates/<same path>` replaces the embedded one (docs/03 §3.1);
//! overrides are read on every render, so edits apply on the next render without
//! restarting the daemon. Rules:
//! - autoescape is off: the output is config text, not HTML;
//! - undefined variables are errors, so a typo never renders an empty directive;
//! - output is always UTF-8 without BOM, LF, one trailing newline.
//!
//! Rendering is CPU-bound and reads override files: call it from `spawn_blocking`.

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::paths::ForgeRoot;
use include_dir::{Dir, include_dir};
use minijinja::syntax::SyntaxConfig;
use minijinja::value::Serde;
use minijinja::{Environment, ErrorKind, UndefinedBehavior, Value};
use serde::Serialize;

use crate::managed::normalize_text;

static EMBEDDED: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Template renderer with embedded templates and an optional override directory.
#[derive(Debug, Clone, Default)]
pub struct Engine {
    override_dir: Option<PathBuf>,
}

impl Engine {
    /// Engine that only uses the embedded templates.
    pub fn embedded_only() -> Self {
        Self { override_dir: None }
    }

    /// Engine that honours `<root>/etc/templates/` overrides.
    pub fn for_root(root: &ForgeRoot) -> Self {
        Self::with_override_dir(root.etc_templates())
    }

    /// Engine with an explicit override directory.
    pub fn with_override_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            override_dir: Some(dir.into()),
        }
    }

    /// Names (relative, `/`-separated) of every embedded template.
    pub fn embedded_template_names() -> Vec<String> {
        fn walk(dir: &Dir<'static>, out: &mut Vec<String>) {
            for file in dir.files() {
                out.push(file.path().to_string_lossy().replace('\\', "/"));
            }
            for sub in dir.dirs() {
                walk(sub, out);
            }
        }
        let mut names = Vec::new();
        walk(&EMBEDDED, &mut names);
        names.sort();
        names
    }

    /// Source of template `name`: the override when present, else the embedded one.
    pub fn source(&self, name: &str) -> Result<Option<String>> {
        load_source(self.override_dir.as_deref(), name)
    }

    /// Renders template `name` (for example `nginx/nginx.conf.j2`) with `ctx`.
    pub fn render<S: Serialize>(&self, name: &str, ctx: &S) -> Result<String> {
        if !safe_relative(name) {
            return Err(ForgeError::InvalidInput(format!(
                "invalid template name '{name}'"
            )));
        }
        let env = self.environment()?;
        let template = env.get_template(name).map_err(|e| map_error(name, &e))?;
        let out = template
            .render(Value::from(Serde(ctx)))
            .map_err(|e| map_error(name, &e))?;
        Ok(normalize_text(&out))
    }

    fn environment(&self) -> Result<Environment<'static>> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        let syntax = SyntaxConfig::builder()
            .trim_blocks(true)
            .lstrip_blocks(true)
            .keep_trailing_newline(true)
            .build()
            .map_err(|e| ForgeError::Internal(format!("template syntax: {e}")))?;
        env.set_syntax(syntax);
        let dir = self.override_dir.clone();
        env.set_loader(move |name| {
            load_source(dir.as_deref(), name)
                .map_err(|e| minijinja::Error::new(ErrorKind::InvalidOperation, e.to_string()))
        });
        env.add_filter("win_path", win_path);
        env.add_filter("posix_path", posix_path);
        env.add_filter("quote", quote);
        env.add_filter("indent", indent);
        env.add_filter("to_yaml", to_yaml);
        Ok(env)
    }
}

fn map_error(name: &str, e: &minijinja::Error) -> ForgeError {
    if e.kind() == ErrorKind::TemplateNotFound {
        return ForgeError::NotFound(format!("template {name}"));
    }
    let mut msg = format!("template {name}: {e}");
    if let Some(line) = e.line() {
        msg.push_str(&format!(" (line {line})"));
    }
    ForgeError::ConfigInvalid(msg)
}

fn safe_relative(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && name.split('/').all(|part| !part.is_empty() && part != "..")
}

fn load_source(override_dir: Option<&Path>, name: &str) -> Result<Option<String>> {
    if !safe_relative(name) {
        return Err(ForgeError::InvalidInput(format!(
            "invalid template name '{name}'"
        )));
    }
    if let Some(dir) = override_dir {
        let path = dir.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).map_err(|_| {
                    ForgeError::ConfigInvalid(format!("{} is not valid UTF-8", path.display()))
                })?;
                return Ok(Some(text));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ForgeError::io_at("reading", &path, e)),
        }
    }
    Ok(EMBEDDED
        .get_file(name)
        .and_then(|f| f.contents_utf8())
        .map(str::to_owned))
}

/// `C:/a/b` → `C:\a\b`.
fn win_path(value: String) -> String {
    value.replace('/', "\\")
}

/// `C:\a\b` → `C:/a/b`.
fn posix_path(value: String) -> String {
    value.replace('\\', "/")
}

/// Wraps in double quotes, escaping only `"` (backslashes are kept: they are regex or
/// path text in every supported config syntax, and paths are already POSIX).
fn quote(value: String) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

/// Indents every non-empty line by `width` spaces (first line included, unlike Jinja's
/// default, because templates place the filter at the start of a line).
fn indent(value: String, width: usize) -> String {
    let pad = " ".repeat(width);
    value
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn to_yaml(value: Value) -> std::result::Result<String, minijinja::Error> {
    serde_yaml_ng::to_string(&value)
        .map(|s| s.trim_end().to_owned())
        .map_err(|e| minijinja::Error::new(ErrorKind::InvalidOperation, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn every_embedded_template_is_utf8_lf_without_bom() {
        let names = Engine::embedded_template_names();
        assert!(names.len() >= 20, "{names:?}");
        for name in names {
            let src = Engine::embedded_only().source(&name).unwrap().unwrap();
            assert!(!src.contains('\r'), "{name} has CR");
            assert!(!src.starts_with('\u{feff}'), "{name} has BOM");
            assert!(!src.contains("~*"), "{name} uses case-insensitive regex");
        }
    }

    #[test]
    fn filters_work() {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::with_override_dir(dir.path());
        std::fs::write(
            dir.path().join("t.j2"),
            "{{ a | win_path }}|{{ b | posix_path }}|{{ c | quote }}|\n{{ d | indent(2) }}\n{{ y | to_yaml }}",
        )
        .unwrap();
        let out = e
            .render(
                "t.j2",
                &json!({"a": "C:/x/y", "b": "C:\\x\\y", "c": "a\"b", "d": "l1\n\nl2", "y": {"k": [1, 2]}}),
            )
            .unwrap();
        assert_eq!(
            out,
            "C:\\x\\y|C:/x/y|\"a\\\"b\"|\n  l1\n\n  l2\nk:\n- 1\n- 2\n"
        );
    }

    #[test]
    fn autoescape_is_off_and_undefined_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::with_override_dir(dir.path());
        std::fs::write(dir.path().join("a.j2"), "{{ v }}").unwrap();
        assert_eq!(e.render("a.j2", &json!({"v": "<&>"})).unwrap(), "<&>\n");
        let err = e.render("a.j2", &json!({})).unwrap_err();
        assert_eq!(err.code(), "config_invalid");
    }

    #[test]
    fn output_is_normalized() {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::with_override_dir(dir.path());
        std::fs::write(dir.path().join("a.j2"), "\u{feff}x\r\ny\r\n\r\n").unwrap();
        // The BOM is part of the template text: it must not survive in the output.
        assert_eq!(e.render("a.j2", &json!({})).unwrap(), "x\ny\n");
    }

    #[test]
    fn overrides_take_precedence_and_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::with_override_dir(dir.path());
        let name = "nginx/snippets/php-common.conf.j2";
        let embedded = e.render(name, &json!({})).unwrap();
        assert!(embedded.contains("fastcgi_keep_conn"));
        std::fs::create_dir_all(dir.path().join("nginx/snippets")).unwrap();
        std::fs::write(dir.path().join(name), "# mine {{ x }}\n").unwrap();
        assert_eq!(e.render(name, &json!({"x": 1})).unwrap(), "# mine 1\n");
        // Other templates still come from the embedded set.
        assert!(
            e.render(
                "nginx/snippets/ssl.conf.j2",
                &json!({"site": {"ssl": {"cert": "a", "key": "b"}}})
            )
            .unwrap()
            .contains("ssl_certificate \"a\"")
        );
        // Removing the override restores the embedded one.
        std::fs::remove_file(dir.path().join(name)).unwrap();
        assert_eq!(e.render(name, &json!({})).unwrap(), embedded);
    }

    #[test]
    fn unknown_and_unsafe_names() {
        let e = Engine::embedded_only();
        assert_eq!(
            e.render("nope.j2", &json!({})).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            e.render("../secret", &json!({})).unwrap_err().code(),
            "invalid_input"
        );
    }
}
