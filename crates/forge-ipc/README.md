# forge-ipc

**O que é:** Contratos da API em JSON-RPC 2.0 (serde), transporte abstrato (named pipe e futuro WebSocket), eventos com `seq` e cliente.

**O que não é:** Não contém lógica de domínio: só tipos e transporte. GUI e CLI dependem somente deste crate.

**Quem depende dele:** `forge-daemon`, `forge-cli`, `forge-mcp` e o app desktop (quando existir).

**Referência:** [docs/09-cli.md](../../docs/09-cli.md), [docs/13-remote-mode.md](../../docs/13-remote-mode.md) e [ADR-0009](../../docs/adr/0009-remote-ready-ipc.md).
