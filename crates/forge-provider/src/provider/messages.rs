//! User-facing error strings (pt-BR). Kept apart until the i18n infrastructure exists
//! (docs/09 §i18n); then each function becomes a message key.

pub(crate) fn offline_no_cache(url: &str) -> String {
    format!("modo offline ativo e sem cache de metadados para {url}")
}

pub(crate) fn rate_limited(url: &str, reset: Option<&str>) -> String {
    match reset {
        Some(reset) => format!(
            "limite de requisições atingido em {url}; tente novamente após {reset} (ou configure um token do GitHub)"
        ),
        None => format!(
            "limite de requisições atingido em {url}; tente mais tarde (ou configure um token do GitHub)"
        ),
    }
}

pub(crate) fn upstream_unavailable(url: &str, reason: &str) -> String {
    format!("não foi possível consultar {url}: {reason}")
}

pub(crate) fn stale_cache(url: &str) -> String {
    format!("usando metadados em cache (possivelmente antigos) de {url}")
}
