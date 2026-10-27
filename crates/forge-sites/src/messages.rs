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

/// Progress text of the Case Mismatch Finder (`job.progress`).
pub fn lint_case_progress(files: u32) -> (&'static str, String) {
    (
        "sites.lint_case.progress",
        format!("Verificando referências de arquivos ({files} arquivos lidos)"),
    )
}

/// Parity note: an existing `.gitattributes`/`.editorconfig` lacks the LF rule; the user
/// is told what to add instead of Forgebox rewriting their file.
pub fn parity_suggestion(file: &str, rule: &str) -> (&'static str, String) {
    (
        "sites.parity.suggestion",
        format!("Sugestão: adicione `{rule}` ao início de {file} para manter LF como no Linux."),
    )
}

/// Parity note: Git is not available, so its settings were not changed.
pub fn parity_git_missing() -> (&'static str, &'static str) {
    (
        "sites.parity.git_missing",
        "Git não encontrado: core.ignorecase e core.autocrlf não foram ajustados.",
    )
}

/// Parity note: the folder is not a Git repository.
pub fn parity_not_a_repo() -> (&'static str, &'static str) {
    (
        "sites.parity.not_a_repo",
        "A pasta não é um repositório Git; nada a ajustar no Git.",
    )
}
