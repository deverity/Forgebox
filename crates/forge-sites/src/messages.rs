//! User-facing messages (pt-BR) with their i18n keys. Until the i18n infrastructure of the
//! CLI/GUI exists (M1-E5/M2), strings live here so they can be moved to catalogs.

/// Warning: the host had uppercase letters and was lowercased.
pub fn host_lowercased(raw: &str, normalized: &str) -> (&'static str, String) {
    (
        "sites.host_lowercased",
        format!("O host '{raw}' foi convertido para minúsculas: '{normalized}'."),
    )
}

/// Error text: host outside the configured TLDs.
pub fn host_tld(host: &str, tlds: &[String]) -> (&'static str, String) {
    (
        "sites.host_tld",
        format!(
            "O host '{host}' deve terminar em um dos TLDs configurados: {}.",
            tlds.iter()
                .map(|t| format!(".{t}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// Error text: malformed host.
pub fn host_invalid(host: &str, why: &str) -> (&'static str, String) {
    (
        "sites.host_invalid",
        format!("Host inválido '{host}': {why}."),
    )
}
