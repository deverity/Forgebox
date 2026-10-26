# forge-daemon

**O que é:** Binário `forgeboxd.exe`: compõe os crates de domínio, faz o boot, o scheduler e serve a API via `forge-ipc`.

**O que não é:** Não tem GUI nem lógica de CLI. Não contém lógica específica de tool.

**Quem depende dele:** ninguém (é o ponto de composição).

**Referência:** [docs/01-architecture.md](../../docs/01-architecture.md) §5.1 e [ADR-0001](../../docs/adr/0001-rust-user-space-daemon.md).
