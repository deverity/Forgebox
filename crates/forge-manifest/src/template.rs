//! `{{ ... }}` references inside manifest strings (docs/04 §1.4).
//!
//! Manifests only substitute variables: an expression is a dotted variable path with
//! optional filters (`{{ paths.etc | posix_path }}`). Literals, operators and `{% %}`
//! blocks are rejected. Which variables are allowed where is decided by the validator.

/// A variable reference found in a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    /// The expression as written, trimmed (`paths.etc | posix_path`).
    pub expr: String,
    /// Variable path (`["paths", "etc"]`).
    pub segments: Vec<String>,
}

impl VarRef {
    /// `paths.etc`.
    pub fn path(&self) -> String {
        self.segments.join(".")
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every `{{ ... }}` of `text`, in order; malformed ones are `Err(reason)`.
pub fn scan(text: &str) -> Vec<Result<VarRef, String>> {
    let mut out = Vec::new();
    if text.contains("{%") {
        out.push(Err(
            "control blocks `{% ... %}` are not allowed in manifests".to_owned(),
        ));
    }
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push(Err("unclosed `{{`".to_owned()));
            break;
        };
        out.push(parse_expr(after[..end].trim()));
        rest = &after[end + 2..];
    }
    out
}

fn parse_expr(expr: &str) -> Result<VarRef, String> {
    let not_a_variable =
        || format!("`{{{{ {expr} }}}}` must be a variable reference such as `{{{{ paths.etc }}}}`");
    let mut parts = expr.split('|');
    let var = parts.next().unwrap_or_default().trim();
    let segments: Vec<String> = var.split('.').map(str::to_owned).collect();
    if var.is_empty() || !segments.iter().all(|s| is_ident(s)) {
        return Err(not_a_variable());
    }
    for filter in parts {
        let name = filter.split('(').next().unwrap_or_default().trim();
        if !is_ident(name) {
            return Err(format!("invalid filter in `{{{{ {expr} }}}}`"));
        }
    }
    Ok(VarRef {
        expr: expr.to_owned(),
        segments,
    })
}

/// If `text` starts with `{{ paths.<dir> }}`, returns `<dir>` and the rest of the text.
pub fn root_dir_prefix(text: &str) -> Option<(String, &str)> {
    let after = text.strip_prefix("{{")?;
    let end = after.find("}}")?;
    let var = parse_expr(after[..end].trim()).ok()?;
    if var.expr.contains('|') {
        return None;
    }
    match var.segments.as_slice() {
        [root, dir] if root == "paths" => Some((dir.clone(), &after[end + 2..])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_variables_and_filters() {
        let refs = scan("{{ paths.etc }}/php/{{series}}/x {{ paths.etc | posix_path }}");
        let paths: Vec<String> = refs.into_iter().map(|r| r.unwrap().path()).collect();
        assert_eq!(paths, ["paths.etc", "series", "paths.etc"]);
        assert!(scan("no templates, ${PORT} and {version}").is_empty());
    }

    #[test]
    fn rejects_expressions_and_blocks() {
        for bad in [
            "{{ 1 + 2 }}",
            "{{ \"x\" }}",
            "{{ }}",
            "{{ a..b }}",
            "{{ a-b }}",
            "{{ paths.etc | 3 }}",
            "{{ open",
            "{% if x %}y{% endif %}",
        ] {
            assert!(scan(bad).iter().any(Result::is_err), "{bad}");
        }
    }

    #[test]
    fn root_dir_prefix_reads_leading_paths_variable() {
        let (dir, rest) = root_dir_prefix("{{ paths.etc }}/php/x.ini").unwrap();
        assert_eq!((dir.as_str(), rest), ("etc", "/php/x.ini"));
        assert_eq!(root_dir_prefix("{{paths.version}}").unwrap().0, "version");
        assert!(root_dir_prefix("C:/x").is_none());
        assert!(root_dir_prefix("{{ series }}/x").is_none());
        assert!(root_dir_prefix("{{ paths.etc | win_path }}/x").is_none());
        assert!(root_dir_prefix(" {{ paths.etc }}").is_none());
    }
}
