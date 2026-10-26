# forge-core

**O que é:** Base do domínio: `ForgeRoot` e todos os caminhos da raiz (`paths`), IDs (`ToolId` slug + newtypes ULID, `ids`), `ForgeError` com códigos estáveis (`error`), `Event`/`EventBus` com `seq` (`event`), store SQLite com migrations (`store`), `forgebox.toml` (`config`) e traits de plataforma (`platform`: `LinkFs`, `CaseSensitiveFs`, `ProcessGroup`, `TrustStore`, `DnsPolicy`).

**O que não é:** Não inicia processos e não fala com rede ou IPC. Chamadas Win32 ficam só em `src/platform/windows/` (junction via crate `junction`; flag case-sensitive via `NtSetInformationFile`). Tudo que toca disco/Win32 é bloqueante: em código async, use `spawn_blocking`.

**Quem depende dele:** todos os crates de domínio (`forge-manifest`, `forge-supervisor`, `forge-render`, `forge-net`, `forge-tls`, `forge-ipc`, `forge-metrics`) e `forge-helper`.

**Notas:** feature `specta` (ligada por `forge-ipc`, que re-exporta `RequestId`) deriva os tipos TS dos IDs. Testes de integração em `tests/` usam junction e NTFS case-sensitive reais; `FORGEBOX_REQUIRE_CASE_SENSITIVE=1` transforma o "skip" em falha.

**Referência:** [docs/01](../../docs/01-architecture.md) §3-§7, [docs/02](../../docs/02-project-structure.md), [docs/08](../../docs/08-release-layout.md) §2, [docs/15](../../docs/15-linux-parity-case-sensitivity.md) §3.
