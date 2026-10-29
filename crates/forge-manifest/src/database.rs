//! Rendering of the `spec.database` capability block (docs/04 §1.8): turns the SQL
//! templates and client runners into exact `(exe, argv)` pairs.
//!
//! Pure string work with no process spawning: the daemon (M2-E3-T5) runs the result. The
//! database name and charset are validated here, before any substitution, so a value can
//! never smuggle quoting characters into a statement or an argument.

use std::collections::BTreeMap;

use crate::schema::tool::{ClientCommand, DatabaseSpec};

/// Longest database name accepted.
pub const MAX_DATABASE_LEN: usize = 64;
/// Longest charset name accepted.
pub const MAX_CHARSET_LEN: usize = 32;

/// Why a template could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("database name {0:?} must match [A-Za-z0-9_]{{1,64}}")]
    InvalidDatabase(String),
    #[error("charset {0:?} must match [A-Za-z0-9_]{{1,32}}")]
    InvalidCharset(String),
    #[error("template variable `{0}` has no value here")]
    MissingValue(String),
    #[error("malformed template: {0}")]
    Malformed(String),
}

/// Values of the variables a database template may use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbVars {
    /// `{{ database }}`.
    pub database: Option<String>,
    /// `{{ charset }}`; the spec's `default_charset` when `None`.
    pub charset: Option<String>,
    /// `instance.*` without the prefix: `ports.main`, `name`, `data_dir`.
    pub instance: BTreeMap<String, String>,
}

impl DbVars {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn database(mut self, name: impl Into<String>) -> Self {
        self.database = Some(name.into());
        self
    }

    pub fn charset(mut self, charset: impl Into<String>) -> Self {
        self.charset = Some(charset.into());
        self
    }

    /// `{{ instance.ports.<name> }}`.
    pub fn port(mut self, name: &str, port: u16) -> Self {
        self.instance
            .insert(format!("ports.{name}"), port.to_string());
        self
    }
}

/// A program and its argv, ready for a spawner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCommand {
    /// Relative to the version folder.
    pub exe: String,
    pub args: Vec<String>,
}

/// The three statements of `spec.database.queries`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbQuery {
    List,
    Create,
    Drop,
}

/// `[A-Za-z0-9_]{1,max}`.
fn is_ident(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

type Lookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Replaces every `{{ name }}` of `text` with `lookup(name)`; the substituted values are
/// never scanned again.
fn substitute(text: &str, lookup: &Lookup<'_>) -> Result<String, RenderError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| RenderError::Malformed(format!("unclosed `{{{{` in {text:?}")))?;
        let name = after[..end].trim();
        let value = lookup(name).ok_or_else(|| RenderError::MissingValue(name.to_owned()))?;
        out.push_str(&value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn render_command(
    command: &ClientCommand,
    lookup: &Lookup<'_>,
) -> Result<RenderedCommand, RenderError> {
    Ok(RenderedCommand {
        exe: command.exe.clone(),
        args: command
            .args
            .iter()
            .map(|arg| substitute(arg, lookup))
            .collect::<Result<_, _>>()?,
    })
}

impl DatabaseSpec {
    fn lookup<'a>(
        &'a self,
        vars: &'a DbVars,
        with_charset: bool,
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| match name {
            "database" => vars.database.clone(),
            "charset" if with_charset => Some(
                vars.charset
                    .clone()
                    .unwrap_or_else(|| self.default_charset.clone()),
            ),
            _ => name
                .strip_prefix("instance.")
                .and_then(|key| vars.instance.get(key).cloned()),
        }
    }

    fn check(&self, vars: &DbVars) -> Result<(), RenderError> {
        if let Some(db) = &vars.database
            && !is_ident(db, MAX_DATABASE_LEN)
        {
            return Err(RenderError::InvalidDatabase(db.clone()));
        }
        if let Some(charset) = &vars.charset
            && !is_ident(charset, MAX_CHARSET_LEN)
        {
            return Err(RenderError::InvalidCharset(charset.clone()));
        }
        Ok(())
    }

    /// The statement of `query` with `{{ database }}`/`{{ charset }}` replaced.
    pub fn render_sql(&self, query: DbQuery, vars: &DbVars) -> Result<String, RenderError> {
        self.check(vars)?;
        let template = match query {
            DbQuery::List => &self.queries.list,
            DbQuery::Create => &self.queries.create,
            DbQuery::Drop => &self.queries.drop,
        };
        substitute(template, &self.lookup(vars, true))
    }

    /// The client invocation that runs `query`: the statement goes into the argument that
    /// holds `{{ sql }}`.
    pub fn render_query(
        &self,
        query: DbQuery,
        vars: &DbVars,
    ) -> Result<RenderedCommand, RenderError> {
        let sql = self.render_sql(query, vars)?;
        self.render_sql_command(&sql, vars)
    }

    /// The runner with an already rendered statement.
    pub fn render_sql_command(
        &self,
        sql: &str,
        vars: &DbVars,
    ) -> Result<RenderedCommand, RenderError> {
        self.check(vars)?;
        let inner = self.lookup(vars, false);
        let lookup = |name: &str| {
            if name == "sql" {
                Some(sql.to_owned())
            } else {
                inner(name)
            }
        };
        render_command(&self.client, &lookup)
    }

    /// `dump`, `restore` or `shell` (the caller picks the field; `None` means the
    /// manifest does not declare it).
    pub fn render_command(
        &self,
        command: &ClientCommand,
        vars: &DbVars,
    ) -> Result<RenderedCommand, RenderError> {
        self.check(vars)?;
        render_command(command, &self.lookup(vars, false))
    }

    /// `url` with the database and instance ports filled in.
    pub fn render_url(&self, vars: &DbVars) -> Result<String, RenderError> {
        self.check(vars)?;
        substitute(&self.url, &self.lookup(vars, false))
    }

    /// `env` of the block, rendered.
    pub fn render_env(&self, vars: &DbVars) -> Result<BTreeMap<String, String>, RenderError> {
        self.check(vars)?;
        let lookup = self.lookup(vars, false);
        self.env
            .iter()
            .map(|(key, value)| Ok((key.clone(), substitute(value, &lookup)?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_does_not_rescan_values() {
        let lookup = |name: &str| (name == "a").then(|| "{{ b }}".to_owned());
        assert_eq!(substitute("x {{ a }} y", &lookup).unwrap(), "x {{ b }} y");
        assert_eq!(
            substitute("{{ b }}", &lookup),
            Err(RenderError::MissingValue("b".to_owned()))
        );
        assert!(matches!(
            substitute("{{ a", &lookup),
            Err(RenderError::Malformed(_))
        ));
    }

    #[test]
    fn identifiers_are_strict() {
        assert!(is_ident("loja_2", 64));
        for bad in [
            "",
            "lo ja",
            "loja;drop",
            "a`b",
            "a\"b",
            "é",
            &"x".repeat(65),
        ] {
            assert!(!is_ident(bad, 64), "{bad}");
        }
    }
}
