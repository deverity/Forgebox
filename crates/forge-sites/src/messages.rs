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

/// Error text: the upstream of a proxy site is not usable.
pub fn upstream_invalid(upstream: &str, why: &str) -> (&'static str, String) {
    (
        "sites.upstream_invalid",
        format!("Upstream inválido '{upstream}': {why}."),
    )
}

/// Error text: a runtime tool has no installed version.
pub fn runtime_not_installed(tool: &str) -> (&'static str, String) {
    (
        "apps.runtime_not_installed",
        format!(
            "Nenhuma versão de '{tool}' está instalada. Instale uma com `forge tool install {tool}`."
        ),
    )
}

/// Error text: the version selector of a runtime is not a version.
pub fn runtime_selector_invalid(tool: &str, selector: &str) -> (&'static str, String) {
    (
        "apps.runtime_selector_invalid",
        format!(
            "Versão '{selector}' de '{tool}' não é entendida: use um número como 22, 22.12 ou v22.12.0 (aliases como lts/iron não são suportados)."
        ),
    )
}

/// Error text: the requested version of a runtime is not installed.
pub fn runtime_version_missing(
    tool: &str,
    selector: &str,
    installed: &[&str],
) -> (&'static str, String) {
    let have = if installed.is_empty() {
        "nenhuma".to_owned()
    } else {
        installed.join(", ")
    };
    (
        "apps.runtime_version_missing",
        format!(
            "'{tool}' {selector} não está instalado (instaladas: {have}). Instale com `forge tool install {tool}@{selector}`."
        ),
    )
}

/// Error text: a process name is not valid.
pub fn process_name_invalid(name: &str) -> (&'static str, String) {
    (
        "apps.process_name_invalid",
        format!("Nome de processo inválido '{name}': use letras minúsculas, números, '-' e '_'."),
    )
}

/// Error text: the folder of a process leaves the project.
pub fn process_cwd_outside(name: &str, cwd: &str) -> (&'static str, String) {
    (
        "apps.process_cwd_outside",
        format!("O processo '{name}' usa cwd '{cwd}', fora da pasta do projeto."),
    )
}

/// Error text: two processes want to proxy the whole host.
pub fn process_proxy_twice(host: &str, first: &str, second: &str) -> (&'static str, String) {
    (
        "apps.process_proxy_twice",
        format!(
            "O site '{host}' tem dois processos com proxy: true ('{first}' e '{second}'); só um pode receber o host inteiro."
        ),
    )
}

/// Error text: an app site without a process to proxy to.
pub fn app_without_proxy(host: &str) -> (&'static str, String) {
    (
        "apps.app_without_proxy",
        format!("O site '{host}' é do tipo app, mas nenhum processo tem proxy: true."),
    )
}

/// Texts of the "starting" page (`/_forgebox/starting`, docs/06 §4.2 and §8).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StartingTexts {
    pub lang: &'static str,
    pub title: &'static str,
    pub heading: &'static str,
    pub body: &'static str,
    pub hint: &'static str,
}

/// Texts of the starting page for `lang` (`pt-BR` or `en-US`; anything else, including
/// `pt`, `en-GB`..., is matched by its primary subtag, falling back to pt-BR).
pub fn starting_texts(lang: &str) -> StartingTexts {
    let primary = lang
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if primary == "en" {
        StartingTexts {
            lang: "en-US",
            title: "Starting...",
            heading: "Starting your app",
            body: "Forgebox is starting the process behind this site. This page reloads by itself.",
            hint: "The first start can take a few seconds.",
        }
    } else {
        StartingTexts {
            lang: "pt-BR",
            title: "Iniciando...",
            heading: "Iniciando o seu app",
            body: "O Forgebox está subindo o processo por trás deste site. Esta página recarrega sozinha.",
            hint: "A primeira subida pode levar alguns segundos.",
        }
    }
}

/// Text under the host on the starting page when the process failed to start.
pub fn starting_failed(process: &str, reason: &str) -> (&'static str, String) {
    (
        "apps.starting_failed",
        format!("O processo '{process}' não subiu: {reason}"),
    )
}
