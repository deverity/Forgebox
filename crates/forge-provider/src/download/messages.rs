//! User-facing progress strings (pt-BR). Kept apart until the i18n infrastructure
//! exists (docs/09 §i18n); then each function becomes a message key.

pub(crate) fn downloading_from(host: &str) -> String {
    format!("Baixando de {host}")
}

pub(crate) fn resuming_from(host: &str, bytes: u64) -> String {
    format!("Retomando de {host} a partir de {bytes} bytes")
}

pub(crate) fn verifying_existing() -> String {
    "Verificando dados já baixados".to_owned()
}

pub(crate) fn verifying() -> String {
    "Verificando sha256".to_owned()
}

pub(crate) fn retrying(attempt: u32, max: u32) -> String {
    format!("Conexão interrompida, tentando novamente ({attempt}/{max})")
}

pub(crate) fn next_mirror(host: &str) -> String {
    format!("Fonte indisponível, tentando {host}")
}
