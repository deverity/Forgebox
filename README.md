# Forgebox

**A stack local inteira, sob controle total, com um clique.**

Ambiente de desenvolvimento local para Windows (PHP, Node, bancos, cache, mail, túneis) escrito em Rust: motor pequeno e determinístico, múltiplas versões de tudo rodando ao mesmo tempo (cada vhost escolhe seu PHP), catálogo aberto de ferramentas em YAML com instalação sob demanda e auto-update, DNS `*.test` e HTTPS locais de verdade, e uma interface com acabamento de app Apple.

Pensado para superar Laragon, Laravel Herd, ServBay, XAMPP e WampServer em função, controle e UI/UX.

## Status
Fase de especificação. Toda a arquitetura, o motor, o padrão de manifestos, o layout de release e o roadmap estão documentados em [`docs/`](docs/README.md). Implementação começa pelo [roadmap](docs/10-roadmap-and-agent-tasks.md).

## Documentação
- [Visão e análise competitiva](docs/00-vision.md)
- [Arquitetura](docs/01-architecture.md) · [Estrutura do repositório](docs/02-project-structure.md) · [Motor](docs/03-engine.md)
- [Manifesto YAML de tools](docs/04-tool-manifest-spec.md) · [Providers e auto-update](docs/05-providers-and-updates.md)
- [Sites, multi-PHP, proxy, DNS, SSL](docs/06-sites-vhosts-multi-php.md) · [Catálogo e compatibilidade](docs/12-stacks-compat.md)
- [UI/UX](docs/07-ui-ux.md) · [Layout do release](docs/08-release-layout.md) · [CLI](docs/09-cli.md) · [Segurança](docs/11-security.md)
- [Modo remoto](docs/13-remote-mode.md) · [Observabilidade por vhost](docs/14-observability-metrics.md) · [Paridade Linux / case-sensitive](docs/15-linux-parity-case-sensitivity.md)
- [ADRs](docs/adr/README.md)

## Licença
MIT (núcleo). Ferramentas de terceiros mantêm suas licenças.

## Desenvolvimento

Pré-requisitos: Windows 10 (1809+) x64. O `rust-toolchain.toml` instala a toolchain stable com `rustfmt` e `clippy`. Para `cargo deny`, instale com `cargo install cargo-deny`.

```
cargo build --workspace                                    # compila todos os crates e binários
cargo test --workspace                                     # testes unitários e de integração
cargo clippy --workspace --all-targets -- -D warnings      # lint, warnings são erro
cargo fmt --all -- --check                                 # formatação
cargo deny check                                           # licenças, advisories, bans e fontes
cargo xtask schema | gen-ts | lint-registry | registry-index # tarefas de desenvolvimento (placeholders até M0-E1-T2)
```

### Espaço em disco (`target/`)

Cada worktree do workflow (`C:\apps_src\Forgebox-wt\E*`) compila no **seu próprio** `target/`. Já tentamos compartilhar `C:\apps_src\Forgebox\target` entre eles e não funciona: o cargo calcula o hash dos pacotes de caminho relativo à raiz do workspace, então crates de branches diferentes recebem o mesmo nome de artefato e um worktree acaba linkando o código do outro (e binários MSVC como `deps/xtask.exe` nem têm hash). Para o custo em disco ficar aceitável:

- `.cargo/config.toml` (repo) e `C:\apps_src\Forgebox-wt\.cargo\config.toml` definem `[profile.dev] incremental = false`, `debug = "line-tables-only"` e `debug = false` para dependências externas. Mantenha os dois iguais. Backtraces continuam com arquivo e linha; quem quiser incremental local usa `CARGO_INCREMENTAL=1`.
- **Workers/agentes:** não alterem esses profiles nem passem `RUSTFLAGS`/`--profile` custom, não rodem `cargo clean` fora do seu worktree, não matem processos de build de outros worktrees e não registrem tarefas agendadas ou serviços. Se o disco tiver menos de 10 GB livres, parem e reportem.
- `scripts/clean-target.ps1` (opcional; `-Register` cria a tarefa agendada "Forgebox target sweep" para o usuário atual) remove `incremental/`, artefatos sem escrita há 5 dias e, abaixo de 30 GB livres, os diretórios de profile inteiros de um `target/`. Log em `target/clean-target.log`.

Antes de começar uma tarefa, leia [CLAUDE.md](CLAUDE.md) e o [índice da documentação](docs/README.md). O estado das tarefas do roadmap fica em [docs/STATUS.md](docs/STATUS.md).
