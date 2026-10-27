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

// ------------------------------------------------------------- updates (M1-E2-T5)

pub(crate) fn update_downloading(tool: &str, version: &str) -> String {
    format!("Baixando {tool} {version}")
}

pub(crate) fn update_installing(tool: &str, version: &str) -> String {
    format!("Instalando {tool} {version} lado a lado")
}

pub(crate) fn update_migrating() -> String {
    "Levando extensões e arquivos da versão anterior".to_owned()
}

pub(crate) fn update_rendering() -> String {
    "Regerando a configuração da série".to_owned()
}

pub(crate) fn update_switching(series: &str) -> String {
    format!("Trocando os pools da série {series} (blue/green)")
}

pub(crate) fn update_retention() -> String {
    "Aplicando a retenção de versões antigas".to_owned()
}

/// A newer build of the series has other provider-derived variants (compiler) while
/// `update.migrate` copies files built for the old one.
pub(crate) fn update_blocked_build(from: &str, to: &str) -> String {
    format!(
        "Atualização bloqueada: {to} é um build diferente de {from} (ex.: outro compilador); \
         extensões copiadas da versão atual podem não carregar. Instale {to} e reinstale as \
         extensões manualmente."
    )
}

pub(crate) fn update_from_changed(version: &str) -> String {
    format!("A versão {version} não está mais instalada; verifique as atualizações de novo.")
}

pub(crate) fn update_in_flight(tool: &str, series: &str) -> String {
    format!("Já existe uma atualização ou rollback de {tool} {series} em andamento.")
}

pub(crate) fn retention_kept_serving(folder: &str) -> String {
    format!("retenção: {folder} mantida (um pool ainda a usa)")
}

pub(crate) fn rollback_nothing(tool: &str, series: &str) -> String {
    format!("Nenhuma atualização de {tool} {series} para desfazer.")
}

pub(crate) fn rollback_not_retained(version: &str) -> String {
    format!("A versão anterior {version} não está mais retida; não há para onde voltar.")
}
