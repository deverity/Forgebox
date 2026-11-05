//! Run-time variables of manifest strings (docs/04 §1.4): what install-time
//! substitution ([`forge_provider::layout::TemplateVars`]) knows — `version`, `series`,
//! `variant.<key>`, `paths.<dir>` — plus what only exists once an instance or a pool runs:
//! `instance.name|data_dir`, `instance.ports.<name>`, `instance.config.<id>`,
//! `prefs.<key>`, for `service.pool` `recycle.*`, `scaling.*` and `{{ port }}` (kept
//! as the placeholder the pool fills per worker), and `{{ value }}` in a quick setting's
//! `also_set` ([`RuntimeVars::with_value`]).
//!
//! Generic on purpose: nothing here knows a tool; the manifest says which variables a
//! string uses and the validator (forge-manifest, rule 5) already refused unknown ones.

use std::collections::BTreeMap;

use forge_core::error::{ForgeError, Result};
use forge_manifest::template;
use forge_provider::layout::TemplateVars;

/// Variables of one instance (`service.daemon`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceVars {
    pub name: String,
    /// `/` separators; empty when the manifest declares no data dir.
    pub data_dir: String,
    /// `instance.ports.<name>`.
    pub ports: BTreeMap<String, u16>,
    /// `instance.config.<id>`: `config.files[].path`, already substituted.
    pub config: BTreeMap<String, String>,
}

/// Variables of a pool (`service.pool`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolVars {
    pub recycle: BTreeMap<String, String>,
    pub scaling: BTreeMap<String, String>,
}

/// Everything a manifest string can reference at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeVars {
    pub base: TemplateVars,
    pub instance: Option<InstanceVars>,
    pub prefs: BTreeMap<String, String>,
    pub pool: Option<PoolVars>,
    /// `{{ value }}`: the written text of a quick setting (`config.quick_settings[].also_set`).
    pub value: Option<String>,
}

impl RuntimeVars {
    pub fn new(base: TemplateVars) -> Self {
        Self {
            base,
            instance: None,
            prefs: BTreeMap::new(),
            pool: None,
            value: None,
        }
    }

    /// These variables plus `{{ value }}` = `value`.
    pub fn with_value(&self, value: impl Into<String>) -> Self {
        Self {
            value: Some(value.into()),
            ..self.clone()
        }
    }

    fn lookup(&self, segments: &[String]) -> std::result::Result<Option<String>, String> {
        let seg: Vec<&str> = segments.iter().map(String::as_str).collect();
        let missing = |what: &str| Err(format!("`{}` is not available: {what}", seg.join(".")));
        let paths = &self.base.paths;
        Ok(Some(match seg.as_slice() {
            ["version"] => self.base.version.clone(),
            ["series"] => self.base.series.clone(),
            ["variant", key] => match self.base.variant.get(*key) {
                Some(v) => v.clone(),
                None => return missing("this build has no such variant"),
            },
            ["paths", key] => match *key {
                "root" => paths.root.clone(),
                "bin" => paths.bin.clone(),
                "etc" => paths.etc.clone(),
                "data" => paths.data.clone(),
                "logs" => paths.logs.clone(),
                "tmp" => paths.tmp.clone(),
                "www" => paths.www.clone(),
                "certs" => paths.certs.clone(),
                "version" => paths.version.clone(),
                _ => return missing("unknown directory"),
            },
            ["instance", rest @ ..] => {
                let Some(instance) = &self.instance else {
                    return missing("no instance in this context");
                };
                match rest {
                    ["name"] => instance.name.clone(),
                    ["data_dir"] => instance.data_dir.clone(),
                    ["ports", name] => match instance.ports.get(*name) {
                        Some(port) => port.to_string(),
                        None => return missing("port not declared or not allocated"),
                    },
                    ["config", id] => match instance.config.get(*id) {
                        Some(path) => path.clone(),
                        None => return missing("no such config file id"),
                    },
                    _ => return missing("unknown instance variable"),
                }
            }
            ["prefs", key] => match self.prefs.get(*key) {
                Some(v) => v.clone(),
                None => return missing("unknown preference"),
            },
            ["port"] if self.pool.is_some() => return Ok(None),
            ["value"] => match &self.value {
                Some(v) => v.clone(),
                None => return missing("only in a quick setting's also_set"),
            },
            ["recycle", key] | ["scaling", key] => {
                let Some(pool) = &self.pool else {
                    return missing("only in service.pool");
                };
                let table = if seg[0] == "recycle" {
                    &pool.recycle
                } else {
                    &pool.scaling
                };
                match table.get(*key) {
                    Some(v) => v.clone(),
                    None => return missing("unknown pool setting"),
                }
            }
            _ => return missing("unknown variable"),
        }))
    }

    /// Replaces every `{{ var }}` / `{{ var | filter }}` of `text` (filters `posix_path`,
    /// `win_path`). `{{ port }}` in a pool context stays as written.
    pub fn substitute(&self, text: &str) -> Result<String> {
        let invalid = |why: String| ForgeError::InvalidInput(format!("{text:?}: {why}"));
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        for found in template::scan(text) {
            let var = found.map_err(invalid)?;
            let start = rest
                .find("{{")
                .ok_or_else(|| invalid("unbalanced template".into()))?;
            let end = rest[start..]
                .find("}}")
                .map(|e| start + e + 2)
                .ok_or_else(|| invalid("unclosed `{{`".into()))?;
            out.push_str(&rest[..start]);
            match self.lookup(&var.segments).map_err(invalid)? {
                None => out.push_str(&rest[start..end]),
                Some(mut value) => {
                    for filter in var.expr.split('|').skip(1).map(str::trim) {
                        match filter {
                            "posix_path" => value = value.replace('\\', "/"),
                            "win_path" => value = value.replace('/', "\\"),
                            other => return Err(invalid(format!("unknown filter `{other}`"))),
                        }
                    }
                    out.push_str(&value);
                }
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// [`RuntimeVars::substitute`] over a list.
    pub fn substitute_all(&self, items: &[String]) -> Result<Vec<String>> {
        items.iter().map(|s| self.substitute(s)).collect()
    }
}

/// Splits a manifest command line (`bin/mysqladmin.exe --port=3306 shutdown`) into
/// program and arguments: whitespace separated, double quotes group (`"a b"`), no escapes
/// (Windows paths keep their backslashes).
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut has = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut current));
                    has = false;
                }
            }
            c => {
                current.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::paths::ForgeRoot;
    use forge_provider::layout::PathVars;

    fn vars() -> RuntimeVars {
        let root = ForgeRoot::new(r"C:\fb").unwrap();
        let base = TemplateVars {
            tool: "mysql".into(),
            version: "8.4.6".into(),
            series: "8.4".into(),
            canonical_name: "8.4.6".into(),
            variant: BTreeMap::new(),
            paths: PathVars::new(&root, &root.bin().join("mysql").join("8.4.6")),
        };
        let mut v = RuntimeVars::new(base);
        v.instance = Some(InstanceVars {
            name: "default".into(),
            data_dir: "C:/fb/data/mysql/default".into(),
            ports: BTreeMap::from([("main".into(), 3306)]),
            config: BTreeMap::from([("my_ini".into(), "C:/fb/etc/mysql/default/my.ini".into())]),
        });
        v.prefs.insert("tld".into(), "test".into());
        v
    }

    #[test]
    fn substitutes_instance_variables() {
        let v = vars();
        assert_eq!(
            v.substitute(
                "--defaults-file={{ instance.config.my_ini }} --port={{instance.ports.main}}"
            )
            .unwrap(),
            "--defaults-file=C:/fb/etc/mysql/default/my.ini --port=3306"
        );
        assert_eq!(
            v.substitute("{{ paths.data }}/mysql/{{ instance.name }} {{ prefs.tld }}")
                .unwrap(),
            "C:/fb/data/mysql/default test"
        );
        assert_eq!(
            v.substitute("{{ paths.version | win_path }}").unwrap(),
            r"C:\fb\bin\mysql\8.4.6"
        );
    }

    #[test]
    fn unknown_or_missing_variables_fail() {
        let v = vars();
        for bad in [
            "{{ instance.ports.http }}",
            "{{ instance.config.nope }}",
            "{{ port }}",
            "{{ recycle.max_requests }}",
            "{{ variant.ts }}",
            "{{ version | upper }}",
        ] {
            assert_eq!(
                v.substitute(bad).unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
    }

    #[test]
    fn pool_context_keeps_the_port_placeholder() {
        let mut v = vars();
        v.instance = None;
        v.pool = Some(PoolVars {
            recycle: BTreeMap::from([("max_requests".into(), "500".into())]),
            scaling: BTreeMap::new(),
        });
        assert_eq!(
            v.substitute("127.0.0.1:{{ port }} {{ recycle.max_requests }}")
                .unwrap(),
            "127.0.0.1:{{ port }} 500"
        );
    }

    #[test]
    fn commands_split_on_whitespace_with_quotes() {
        assert_eq!(
            split_command(r#"bin/mysqladmin.exe  --port=3306 -uroot "shut down""#),
            ["bin/mysqladmin.exe", "--port=3306", "-uroot", "shut down"]
        );
        assert_eq!(
            split_command(r"nginx.exe -p C:\fb\bin"),
            ["nginx.exe", "-p", r"C:\fb\bin"]
        );
        assert!(split_command("   ").is_empty());
    }
}
