# forge-cli

**O que é:** Binário `forge.exe` (lib `forge_cli` + `main.rs`): cliente fino da API do daemon, com saída humana (tabelas `tabled`, cores `anstream` que respeitam `NO_COLOR`, barras `indicatif` para jobs) e `--json` estável (o DTO da API; erros como `{"error":{code,message,details}}`). Todos os grupos e comandos de [docs/09](../../docs/09-cli.md) (árvore `clap` em `src/cli.rs`, um módulo por grupo em `src/commands/`), flags globais `--json --quiet --yes --root --context --no-daemon --no-elevate`, códigos de saída mapeados dos códigos estáveis de erro (2 uso · 3 não encontrado · 4 conflito · 5 elevação · 6 verificação · 1 o resto, inclusive `not_implemented`), daemon iniciado automaticamente (espera até 10 s), `completions` (clap_complete) e `doctor`. Mensagens em pt-BR com chaves tipadas (`src/messages.rs`; `FORGE_LANG=en` para en-US). Decisões em [ADR-0030](../../docs/adr/0030-cli-contract.md).

**O que não é:** Não importa crates de domínio (só `forge-ipc`, que re-exporta os tipos base em `forge_ipc::base`), não toca em processos de serviços nem em arquivos de configuração; nenhum `if tool == ...`.

**Quem depende dele:** ninguém (usuário final e scripts).

**Testes:** unitários nos módulos; `tests/` sobem um `forgeboxd` real numa raiz temporária (`tests/common`) e rodam o binário `forge` com `--root`: `commands.rs` (tabela de todos os comandos, `--json` e humano, mais testes que derivam a cobertura da árvore clap e de `METHODS`), `output.rs` (snapshots com `NO_COLOR`, cores forçadas, `--quiet`, idiomas), `exit_codes.rs`, `lifecycle.rs` (daemon automático, morto e reiniciado, `--no-daemon`, `daemon start|stop|restart`), `misc.rs` (streams, `exec`, `env`, `completions`, perfis remotos, `lint-case`), `online.rs` (`--ignored`: instala o mailpit oficial pelo CLI e o roda). Cada execução do `forge` tem limite de 30 s.

**Referência:** [docs/09-cli.md](../../docs/09-cli.md).
