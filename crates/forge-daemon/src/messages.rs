//! User-facing texts of `forgeboxd` (pt-BR until the i18n infrastructure exists). Logs
//! stay in English; these are only what the daemon prints for a human on stderr.

/// Another daemon already serves this root (exit code 4, conflict).
pub fn already_running(root: &str, mutex: &str) -> String {
    format!(
        "forgeboxd: já existe um daemon rodando para a raiz {root} (mutex {mutex}). \
         Use `forge daemon status` ou `forge daemon stop`."
    )
}

/// The root could not be prepared.
pub fn root_failed(root: &str, error: &str) -> String {
    format!("forgeboxd: não foi possível preparar a raiz {root}: {error}")
}

/// Boot failed.
pub fn boot_failed(error: &str) -> String {
    format!("forgeboxd: falha ao iniciar: {error}")
}

/// A flag of a later milestone.
pub fn not_available(flag: &str, task: &str) -> String {
    format!("forgeboxd: {flag} ainda não está disponível ({task}).")
}

/// Printed in `--foreground` once the API is open.
pub fn ready(endpoint: &str, elapsed_ms: u128) -> String {
    format!("forgeboxd pronto em {elapsed_ms} ms; API em {endpoint}. Ctrl+C para parar.")
}

/// Printed in `--foreground` at the end of a graceful shutdown.
pub fn stopped(elapsed_ms: u128) -> String {
    format!("forgeboxd parado ({elapsed_ms} ms).")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_mention_their_subject() {
        assert!(already_running("C:/fb", r"Global\Forgebox.Daemon").contains("C:/fb"));
        assert!(not_available("--listen", "M6-E3").contains("M6-E3"));
        assert!(ready(r"\\.\pipe\forgebox", 12).contains("12 ms"));
    }
}
