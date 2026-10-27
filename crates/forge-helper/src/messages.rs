//! Texts shown to a person (pt-BR, docs/README.md), kept apart until the i18n
//! infrastructure exists. Responses and `helper.log` stay in English: the UI translates
//! them by `code`.

/// `forgebox-helper --help`.
pub const USAGE: &str = "\
forgebox-helper: executa, como administrador, as operações privilegiadas do Forgebox
(lista fechada: NRPT, hosts, CA em LocalMachine, firewall, Defender, VC++, case-sensitive).
Normalmente é iniciado pelo forgeboxd via UAC; não é um comando para uso direto.

Uso:
  forgebox-helper --request <arquivo> --response <arquivo> [--dry-run]
  forgebox-helper --dry-run --request <arquivo>      (resposta na saída padrão)
  forgebox-helper --dry-run --request -              (pedido na entrada padrão)
  forgebox-helper --version | --help

--dry-run valida o pedido e mostra o plano sem alterar nada no sistema.
Arquivos de troca: <raiz>/state/locks/helper-<request_id>.request.json e .response.json
(criados pelo forgeboxd). Tudo vai para <raiz>/logs/helper.log.

Códigos de saída: 0 = executado; 1 = recusado ou falhou (resposta escrita);
2 = uso incorreto (nenhuma resposta); 3 = a resposta não pôde ser escrita.";

/// Prefix of command line errors on stderr.
pub const USAGE_ERROR: &str = "forgebox-helper: argumentos inválidos";

/// The response could not be written.
pub const RESPONSE_NOT_WRITTEN: &str = "forgebox-helper: não foi possível escrever a resposta";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_lists_every_flag() {
        for flag in [
            "--request",
            "--response",
            "--dry-run",
            "--version",
            "--help",
        ] {
            assert!(USAGE.contains(flag), "{flag}");
        }
        assert!(!USAGE_ERROR.is_empty() && !RESPONSE_NOT_WRITTEN.is_empty());
    }
}
