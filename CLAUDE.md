# CLAUDE.md — Forgebox

Ambiente de desenvolvimento PHP (e stacks web em geral) local para Windows, em Rust. Superar Laragon/Herd/ServBay em função, controle e UI/UX.

## Leia antes de qualquer tarefa
1. `docs/README.md` (índice e convenções).
2. O doc da área da sua tarefa (ver `docs/10-roadmap-and-agent-tasks.md` para a lista de tarefas e seus docs).
3. `docs/adr/` para não re-discutir decisões já tomadas.

## Regras que não se negocia
- **Nomes:** produto Forgebox · CLI `forge` · daemon `forgeboxd` · GUI `Forgebox.exe` · helper `forgebox-helper`.
- **Nada de lógica por tool em Rust.** Tools são manifestos YAML (`docs/04`) + templates (`crates/forge-render/templates`). Se precisar de `if tool.id == ...`, estenda o schema.
- **Win32 só em `crates/forge-core/src/platform/windows/`** atrás de traits.
- **GUI e CLI só dependem de `forge-ipc`.** Toda funcionalidade passa pela API do daemon.
- **Multi-versão é padrão:** `bin/<tool>/<versão>` + junction `current`; serviços web apontam para versão exata; PHP por vhost via pools (`docs/03 §2`, `docs/06 §2`).
- **Nunca** exigir Windows Services para a stack; elevação só via `forgebox-helper` com allowlist (`docs/11`).
- **Downloads sempre verificados** (sha256 obrigatório).
- **Configs em `etc/` usam blocos gerenciados** `# forgebox:begin/end`; fora deles não se escreve.
- **Idioma:** docs/UI em pt-BR (+ en-US), código/identificadores/logs/commits em inglês.
- **Commits:** `M1-E3-T4: short description` (ID da tarefa do roadmap). Um PR por tarefa, com testes.
- **API remote-ready** (`docs/adr/0009`): transporte abstrato, ULIDs, streams com `seq`, `request_id`, `local_only`; nunca assumir que cliente e daemon estão na mesma máquina.
- **Templates de servidor web sempre emitem access log JSON `forgebox`** e `FORGEBOX_SITE` (`docs/14`).
- **Concorrência (`docs/03 §7`, ADR-0011):** cada serviço/pool/subsistema é um ator com mailbox; mensagens sempre com `tokio::time::timeout`; nunca `Mutex` através de `.await`; I/O síncrono e Win32 lento só em `spawn_blocking`; SQLite em thread de escrita dedicada; operações longas devolvem `job_id` e progridem por eventos. Nenhuma tool pode travar o daemon ou a GUI.
- **Case-sensitive por padrão (`docs/15`, ADR-0010):** diretórios criados pelo Forgebox (`www/`, `etc/`, `data/`, `logs/`, `tmp/`) recebem a flag NTFS `FILE_CS_FLAG_CASE_SENSITIVE_DIR`; configs geradas usam regex case-sensitive, LF, UTF-8 sem BOM e `/`; hosts sempre minúsculos. `bin/` fica insensitive.
- Ao divergir de um doc, **atualize o doc no mesmo PR**.

## Stack
Rust 2024 stable · tokio · rusqlite · serde_yaml_ng · minijinja · reqwest(rustls) · hickory-server · rcgen · windows crate · clap · Tauri 2 + React 19 + TS + Tailwind 4 + Radix + Motion · pnpm.

## Comandos (após M0)
```
cargo build --workspace            cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo xtask schema | gen-ts | lint-registry | registry-index
pnpm -C apps/desktop dev | build   cargo tauri dev (em apps/desktop)
```

## Referência local
Uma instalação antiga do Laragon existe em `C:\laragon` (somente leitura para consulta de layout/vhosts/templates). Nunca modificar.
