//! User-facing progress strings (pt-BR) of verification and extraction. Kept apart until
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
