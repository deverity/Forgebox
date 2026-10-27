# forge-net

**O que é:** Alocação de portas (`ports::PortAllocator`: faixas nomeadas do `forgebox.toml`, reservas persistidas na tabela `ports`, checagem de bind para portas excluídas pelo sistema, conflitos com processos externos via `forge_core::platform::PortTable`), servidor DNS para `*.test` (hickory), política NRPT e hosts via helper, e regras de firewall.

**O que não é:** Não eleva privilégios por conta própria: qualquer operação administrativa passa por `forge-helper`. Não publica eventos: o daemon publica `port.conflict` a partir de `PortAllocator::conflicts`. Não é dependência de `forge-supervisor` (as pré-checagens usam `PortTable` de `forge-core` direto).

**Testes:** unitários com tabela TCP falsa em `src/ports.rs`; `tests/ports_windows.rs` usa sockets reais (listener real pulado, checagem de bind, conflito com PID/exe reais, persistência entre reaberturas do `Store`).

**Quem depende dele:** `forge-daemon`.

**Referência:** [docs/03-engine.md](../../docs/03-engine.md) §4, [docs/06-sites-vhosts-multi-php.md](../../docs/06-sites-vhosts-multi-php.md), [docs/11-security.md](../../docs/11-security.md) e [ADR-0004](../../docs/adr/0004-dns-nrpt-resolver.md).
