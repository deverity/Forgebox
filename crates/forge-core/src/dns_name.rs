//! DNS name syntax shared by the configuration (`tld`, `[dns]`), the elevated helper's
//! parameter validation ([`crate::helper`]) and the resolver zones (`forge_net::dns`).
//!
//! Forgebox only deals with lowercase ASCII host names (docs/15 §7: hosts are always
//! lowercase): labels of `[a-z0-9-]`, 1 to 63 characters, not starting or ending with
//! `-`, at most 253 characters in total, no trailing dot. Internationalized names are
//! written in their punycode form (`xn--...`), which fits the same rule.

/// Longest host name (RFC 1035, without the trailing dot).
pub const MAX_NAME_LEN: usize = 253;
/// Longest label.
pub const MAX_LABEL_LEN: usize = 63;

/// Suffixes that are reserved or never delegated in the public DNS, so routing them to a
/// local resolver cannot hijack a real domain: RFC 2606/6761 (`test`, `example`,
/// `invalid`, `localhost`), RFC 6762 (`local`), RFC 8375 (`home.arpa`), ICANN 2024
/// (`internal`) and the name-collision strings ICANN deferred indefinitely (`home`,
/// `corp`) plus the de facto private `lan`.
///
/// The elevated helper only routes (NRPT) or maps (`hosts`) names under these suffixes,
/// whatever `forgebox.toml` says (docs/11 §1, ADR-0022): the file is writable by the user,
/// the machine-wide resolver policy is not.
pub const LOCAL_USE_SUFFIXES: &[&str] = &[
    "test",
    "localhost",
    "invalid",
    "example",
    "local",
    "internal",
    "home.arpa",
    "lan",
    "home",
    "corp",
];

/// Checks one label.
fn check_label(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("empty label".to_owned());
    }
    if label.len() > MAX_LABEL_LEN {
        return Err(format!(
            "label {label:?} is longer than {MAX_LABEL_LEN} characters"
        ));
    }
    if !label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!(
            "label {label:?} may only use lowercase letters, digits and '-'"
        ));
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err(format!("label {label:?} must not start or end with '-'"));
    }
    Ok(())
}

/// Checks a host or zone name without trailing dot (`loja.test`, `hml.test`, `test`).
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name is empty".to_owned());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!("name is longer than {MAX_NAME_LEN} characters"));
    }
    for label in name.split('.') {
        check_label(label).map_err(|e| format!("{name:?}: {e}"))?;
    }
    Ok(())
}

/// Validates a TLD/suffix as written in the configuration and in NRPT namespaces: a
/// leading dot and a valid name (`.test`, `.home.arpa`). Returns the name without the
/// dot (`test`).
pub fn parse_suffix(suffix: &str) -> Result<&str, String> {
    let Some(name) = suffix.strip_prefix('.') else {
        return Err(format!(
            "{suffix:?} must start with a dot and be lowercase (like \".test\")"
        ));
    };
    check_name(name).map_err(|e| format!("{suffix:?}: {e}"))?;
    Ok(name)
}

/// Whether `name` is `zone` itself or a name under it (both without leading/trailing
/// dots, lowercase).
pub fn is_within(name: &str, zone: &str) -> bool {
    name == zone
        || (name.len() > zone.len()
            && name.ends_with(zone)
            && name.as_bytes()[name.len() - zone.len() - 1] == b'.')
}

/// Whether `name` (without leading dot) is under one of [`LOCAL_USE_SUFFIXES`].
pub fn is_local_use(name: &str) -> bool {
    LOCAL_USE_SUFFIXES.iter().any(|s| is_within(name, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in [
            "test",
            "loja.test",
            "a-b.c1.test",
            "xn--caf-dma.test",
            "1.2.3",
        ] {
            check_name(ok).unwrap();
        }
        let long_label = format!("{}.test", "a".repeat(64));
        let long_name = format!("{}.test", vec!["a".repeat(60); 5].join("."));
        for bad in [
            "",
            "Loja.test",
            "loja..test",
            ".loja.test",
            "loja.test.",
            "-loja.test",
            "loja-.test",
            "lo_ja.test",
            "*.test",
            "loja test",
            "café.test",
            &long_label,
            &long_name,
        ] {
            assert!(check_name(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn suffixes() {
        assert_eq!(parse_suffix(".test").unwrap(), "test");
        assert_eq!(parse_suffix(".home.arpa").unwrap(), "home.arpa");
        for bad in ["test", ".", ".TEST", "..test", ".test.", ""] {
            assert!(parse_suffix(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn within_and_local_use() {
        assert!(is_within("loja.test", "test"));
        assert!(is_within("test", "test"));
        assert!(is_within("a.hml.test", "hml.test"));
        assert!(!is_within("latest", "test"), "suffix must be a whole label");
        assert!(!is_within("test", "loja.test"));
        assert!(is_local_use("test"));
        assert!(is_local_use("dev.local"));
        assert!(is_local_use("router.home.arpa"));
        for public in ["com", "dev", "app", "io", "arpa", "testing", "google.com"] {
            assert!(!is_local_use(public), "{public}");
        }
    }
}
