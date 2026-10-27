//! User-facing texts (pt-BR) of the case sensitivity conversion, with their i18n keys.
//! Until the i18n catalogs of the CLI/GUI exist (M1-E5/M2) the strings live here so they
//! can be moved without touching the logic. Log messages stay in English.

use super::convert::ConvertPhase;

/// Short step text for `job.progress` (`message`).
pub fn phase(phase: ConvertPhase) -> (&'static str, &'static str) {
    match phase {
        ConvertPhase::Recovering => (
            "fs.case.phase.recovering",
            "Restaurando uma conversão interrompida",
        ),
        ConvertPhase::Scanning => ("fs.case.phase.scanning", "Analisando a pasta"),
        ConvertPhase::Preparing => ("fs.case.phase.preparing", "Preparando a pasta nova"),
        ConvertPhase::Moving => ("fs.case.phase.moving", "Movendo arquivos"),
        ConvertPhase::Swapping => ("fs.case.phase.swapping", "Trocando as pastas"),
        ConvertPhase::Verifying => ("fs.case.phase.verifying", "Verificando o resultado"),
        ConvertPhase::Cleaning => ("fs.case.phase.cleaning", "Removendo a pasta antiga"),
        ConvertPhase::RollingBack => ("fs.case.phase.rolling_back", "Desfazendo a conversão"),
    }
}

/// Reason of `fs.case_sensitivity_unavailable` when the folder was converted back.
pub fn disabled() -> (&'static str, &'static str) {
    (
        "fs.case.reason.disabled",
        "Modo case-sensitive desligado para esta pasta.",
    )
}

/// Reason when the folder itself is insensitive (never converted).
pub fn insensitive() -> (&'static str, &'static str) {
    (
        "fs.case.reason.insensitive",
        "A pasta não diferencia maiúsculas de minúsculas. Use Converter para ativar a paridade com Linux.",
    )
}

/// Reason when the folder is flagged but some subfolders are not.
pub fn partial(flagged: u64, total: u64) -> (&'static str, String) {
    (
        "fs.case.reason.partial",
        format!(
            "Só {flagged} de {total} pastas verificadas diferenciam maiúsculas de minúsculas. \
             Use Converter para aplicar a toda a árvore."
        ),
    )
}

/// Reason when the folder does not exist.
pub fn missing(dir: &str) -> (&'static str, String) {
    (
        "fs.case.reason.missing",
        format!("A pasta {dir} não existe."),
    )
}

/// Reason when the volume/OS cannot use the flag (`detail` comes from the platform).
pub fn unsupported(detail: &str) -> (&'static str, String) {
    (
        "fs.case.reason.unsupported",
        format!(
            "Este volume ou esta versão do Windows não suporta pastas case-sensitive \
             (NTFS e Windows 10 1803+ são necessários): {detail}"
        ),
    )
}

/// Error text: an entry could not be moved because a process holds it open.
pub fn in_use(path: &str, detail: &str) -> String {
    format!(
        "{path} está em uso por outro processo ({detail}). Feche editores, terminais e \
         serviços que usam a pasta e tente de novo; nada foi alterado."
    )
}

/// Error text: reverting would merge names that differ only by case.
pub fn case_collisions(examples: &str, count: usize) -> String {
    format!(
        "Não é possível voltar para o modo insensitive: {count} nome(s) diferem só por \
         maiúsculas/minúsculas ({examples}). Renomeie ou remova um de cada par e tente de novo."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_has_a_key_and_text() {
        for p in [
            ConvertPhase::Recovering,
            ConvertPhase::Scanning,
            ConvertPhase::Preparing,
            ConvertPhase::Moving,
            ConvertPhase::Swapping,
            ConvertPhase::Verifying,
            ConvertPhase::Cleaning,
            ConvertPhase::RollingBack,
        ] {
            let (key, text) = phase(p);
            assert!(key.starts_with("fs.case.phase."));
            assert!(!text.is_empty());
        }
    }
}
