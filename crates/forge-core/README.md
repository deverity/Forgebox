# forge-core

**O que é:** Tipos de domínio, IDs ULID, erros, eventos, store SQLite, caminhos (`ForgeRoot`) e traits de plataforma.

**O que não é:** Não inicia processos e não fala com rede ou IPC. Chamadas Win32 ficam só em `src/platform/windows/`.

**Quem depende dele:** todos os crates de domínio (`forge-manifest`, `forge-supervisor`, `forge-render`, `forge-net`, `forge-tls`, `forge-ipc`, `forge-metrics`) e `forge-helper`.

**Referência:** [docs/01-architecture.md](../../docs/01-architecture.md) §3 e [docs/02-project-structure.md](../../docs/02-project-structure.md).
