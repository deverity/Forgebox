# forge-ipc

**O que é:** a API do Forgebox: contratos (DTOs por grupo do docs/09 + tabela única `METHODS` com papel, `local_only`, jobs e comandos da CLI), protocolo JSON-RPC 2.0 em NDJSON com `request_id` obrigatório, trait `Transport` (named pipe com DACL do usuário, unix socket mínimo, em memória; WebSocket/TLS placeholder), autenticação por `state/ipc.token` (`system.hello`), servidor (dispatcher, idempotência, streams drop-oldest com `resume_from`, `fs.*` restrito) e cliente tipado com reconexão.

**O que não é:** não contém lógica de domínio: o daemon registra os handlers num `Router` e converte seus tipos nos DTOs. Win32 fica em `forge-core::platform::windows`.

**Quem depende dele:** `forge-daemon`, `forge-cli`, `forge-mcp` e o app desktop (tipos em `apps/desktop/src/ipc/` via `cargo xtask gen-ts`).

**Referência:** [ADR-0015](../../docs/adr/0015-ipc-protocol.md), [ADR-0009](../../docs/adr/0009-remote-ready-ipc.md), [docs/09-cli.md](../../docs/09-cli.md), [docs/13-remote-mode.md](../../docs/13-remote-mode.md), [docs/11-security.md](../../docs/11-security.md) §4.
