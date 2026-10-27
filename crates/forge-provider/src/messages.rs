//! User-facing strings (pt-BR) of verification, extraction and the layout engine
//! (progress, shim errors). Kept apart until
//! the i18n infrastructure exists (docs/09 §i18n); then each function becomes a message
//! key. The download engine keeps its own in `download/messages.rs`.

pub(crate) fn verifying(algorithm: &str) -> String {
    format!("Verificando {algorithm}")
}

pub(crate) fn verifying_signature(kind: &str) -> String {
    format!("Verificando assinatura {kind}")
}

pub(crate) fn extracting(done: u64, total: Option<u64>) -> String {
    match total {
        Some(total) => format!("Extraindo {done}/{total}"),
        None => format!("Extraindo {done} itens"),
    }
}

pub(crate) fn installing_files() -> String {
    "Movendo arquivos para o destino".to_owned()
}

// ------------------------------------------------------------- layout (M1-E2-T4)

pub(crate) fn install_verifying() -> String {
    "Verificando o arquivo baixado".to_owned()
}

pub(crate) fn install_post_install() -> String {
    "Executando as ações pós-instalação".to_owned()
}

pub(crate) fn install_moving(tool: &str, folder: &str) -> String {
    format!("Movendo para bin/{tool}/{folder}")
}

pub(crate) fn install_registering() -> String {
    "Registrando a versão".to_owned()
}

pub(crate) fn install_activating() -> String {
    "Ativando a versão (current)".to_owned()
}

// Shim messages are echoed by cmd.exe: ASCII only (the console code page is not UTF-8)
// and none of `% ! ^ & | < >` except the delayed-expansion references on purpose.

/// No project version and no `current` junction.
pub(crate) fn shim_no_current(tool: &str) -> String {
    format!("Forgebox: nenhuma versao ativa de {tool}. Ative uma com: forge use {tool} VERSAO")
}

/// The project asks for a version that is not installed (`!FB_SEL!`, `!FB_SRC!` are
/// expanded by the shim).
pub(crate) fn shim_not_installed(tool: &str, installed: &str) -> [String; 3] {
    [
        format!("Forgebox: {tool} !FB_SEL! pedido por !FB_SRC! nao esta instalado."),
        format!("Instalados: {installed}. Instale com: forge install {tool}@!FB_SEL!"),
        "O arquivo de versao deve ser texto simples (ASCII ou UTF-8 sem BOM), ex.: 8.3".to_owned(),
    ]
}

/// The selected folder lacks the command (`%FB_EXE%` is expanded by the shim).
pub(crate) fn shim_missing_file() -> String {
    "Forgebox: \"%FB_EXE%\" nao existe.".to_owned()
}
