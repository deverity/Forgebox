# forge-core

**O que é:** Base do domínio: `ForgeRoot` e todos os caminhos da raiz (`paths`), IDs (`ToolId` slug + newtypes ULID, `ids`), `ForgeError` com códigos estáveis (`error`), `Event`/`EventBus` com `seq` (`event`), store SQLite com migrations, thread de escrita e pool de leitura (`store`), `forgebox.toml` (`config`), traits de plataforma (`platform`: `LinkFs`, `CaseSensitiveFs`, `CodeSign` — Authenticode —, `ProcessControl`/`ProcessGroup` — Job Objects, spawn suspenso com stdio capturado, `CTRL_BREAK` via `forgebox-ctrl`, `WM_CLOSE` —, `TrustStore`, `DnsPolicy`) e o esqueleto de concorrência (docs/03 §7, ADR-0011/0014): atores e supervisor (`actor`), jobs (`jobs`), travas ordenadas (`sync`) e runtime tokio (`runtime`).

**O que não é:** Não supervisiona processos (estados, cascata de parada, logs: `forge-supervisor`) e não fala com rede ou IPC; só oferece as primitivas de processo da plataforma. Chamadas Win32 ficam só em `src/platform/windows/` (junction via crate `junction`; flag case-sensitive via `NtSetInformationFile`; Authenticode via `WinVerifyTrust` em `authenticode.rs`; Job Objects/`CreateProcessW`/console/`EnumWindows` em `process.rs`, `console_ctrl.rs`, `window_close.rs`). Tudo que toca disco/Win32 é bloqueante: em código async, use `spawn_blocking`.

**Quem depende dele:** todos os crates de domínio (`forge-manifest`, `forge-supervisor`, `forge-render`, `forge-net`, `forge-tls`, `forge-ipc`, `forge-metrics`) e `forge-helper`.

**Concorrência (resumo):**
- `actor`: `ActorSupervisor::spawn(ActorSpec::new("mysql/default"), || MyActor::new())` → `ActorHandle`; tasks do próprio ator guardam `ctx.myself_weak()` (`WeakActorRef`), que não impede o ator de parar; `handle.actor_ref().ask(Msg::Status, Duration::from_millis(500))` (timeout obrigatório); `tell` nunca espera (`MailboxFull`). Pânico → reinício com backoff e `actor.restarted`; orçamento esgotado → `Failed` e `actor.failed`. A ordem de travas está no topo de `src/actor/mod.rs`.
- `jobs`: `JobRegistry::start("tool.install php@8.3", |ctx| async move { ctx.progress(…); ctx.check_cancelled()?; Ok(resultado) })` devolve o `JobId` na hora; eventos `job.*` com payload versionado (`jobs::payload`).
- `store`: `store.writer().execute(|conn| …).await` (thread `forgebox-db-writer`, lotes de até 64 ops/5 ms com `SAVEPOINT` por op) e `store.reader().query(|conn| …).await` (`spawn_blocking`, conexões `query_only`). Feche com `store.close()` em contexto bloqueante.
- `runtime`: `build_runtime(&RuntimeConfig::default())` — `max(4, núcleos)` workers, `2 × núcleos` threads de bloqueio.

**tokio-console (dev):** feature opt-in `console` + `RUSTFLAGS="--cfg tokio_unstable"`, por exemplo `RUSTFLAGS="--cfg tokio_unstable" cargo run -p forge-daemon --features forge-core/console`, e chamar `forge_core::runtime::init_console()` no início do binário; depois rodar `tokio-console` (porta 6669). O build padrão não exige `tokio_unstable`.

**Notas:** feature `specta` (ligada por `forge-ipc`, que re-exporta `RequestId`) deriva os tipos TS dos IDs; feature `schemars` (ligada por `forge-manifest`) implementa `JsonSchema` para `ToolId` (padrão do slug) nos JSON Schemas dos manifestos. Testes de integração em `tests/` usam junction e NTFS case-sensitive reais; `FORGEBOX_REQUIRE_CASE_SENSITIVE=1` transforma o "skip" em falha.

**Referência:** [docs/01](../../docs/01-architecture.md) §3-§7, [docs/02](../../docs/02-project-structure.md), [docs/03](../../docs/03-engine.md) §7-§8, [docs/08](../../docs/08-release-layout.md) §2, [docs/15](../../docs/15-linux-parity-case-sensitivity.md) §3, [ADR-0011](../../docs/adr/0011-actor-concurrency-model.md), [ADR-0014](../../docs/adr/0014-concurrency-skeleton.md).
