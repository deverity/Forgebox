# Status do roadmap

Fonte: [10-roadmap-and-agent-tasks.md](10-roadmap-and-agent-tasks.md). Estados: `todo | doing | review | done`.
Ao iniciar uma tarefa, marcar `doing` (primeiro commit); ao terminar, `review` (último commit). Só `done` após revisão aprovada.

| ID | Tarefa | Estado | Branch | Observações |
|---|---|---|---|---|
| M0-E1-T1 | Workspace Cargo | done | feature/M0-E1-T1-workspace | 16 crates + xtask com testes; cargo deny ok (warnings de allowances de licença ainda não usadas, previstas); ADR-0012 (serde_yaml_ng) |
| M0-E1-T2 | xtask | done | feature/M0-E1-T2-xtask | schema, gen-ts, lint-registry, ci. Contratos em `forge_ipc::api::contracts` (docs/02: `api/`); specta-serde adicionado (necessário como Format do exporter). CI futuro: rodar `cargo xtask schema` e `gen-ts` e falhar se `git diff --exit-code` acusar mudança. |
| M0-E1-T3 | CI | done | feature/M0-E1-T3-ci | `.github/workflows/ci.yml` (jobs rust + desktop), dependabot, PR template, `scripts/ci.ps1`. Sem push: aceite validado localmente (`scripts/ci.ps1` passa; YAML validado com `actionlint` (exit 0); sem rede para conferir versões das actions, usadas: checkout@v4, rust-toolchain@stable, rust-cache@v2, install-action@v2, pnpm/action-setup@v4, setup-node@v4). `scripts/ci.ps1 -OfflineDeny` permanece como opção; o padrão roda `cargo deny` online (advisory-db em cache, `http.sslBackend=schannel`). No job `desktop`, os passos pnpm só rodam quando o passo `probe` encontra `apps/desktop/package.json` e exige `apps/desktop/pnpm-lock.yaml` (cache do setup-node). |
| M0-E1-T4 | forge-core base | done | feature/M0-E1-T4-forge-core-base | paths/ids/error/event/store/config/platform; junction (crate `junction`) e flag CS (`NtSetInformationFile`) testados de verdade em NTFS/Win10 19045; `RequestId` movido para `forge_core::ids` (feature `specta`); ADR-0013 (jiff); docs/15 §3 corrigido (flag não é recursiva). |
| M0-E1-T5 | Esqueleto de concorrência | done | feature/M0-E1-T5-concurrency-skeleton | forge-core: `actor` (Actor, ActorRef tell/ask com timeout obrigatório, ActorSupervisor com catch_unwind + RestartPolicy + `actor.restarted`/`actor.failed` novo), `jobs` (JobRegistry, `job.*` v1, cancelamento cooperativo + forçado após grace, idempotência por request_id), `store` writer (thread dedicada, lotes 64 ops/5 ms com SAVEPOINT por op) + reader pool `query_only`, `runtime` (feature `console`, exige `--cfg tokio_unstable`), `sync::OrderedMutex` (ordem de travas por debug_assert). ADR-0014. Erros novos `cancelled`/`internal`. Lints `await_holding_lock`/`_refcell_ref`/`_invalid_type` = deny (verificado com snippet). `forgeboxd` ainda não usa `build_runtime`/`init_console` (M1-E5-T2). |
| M1-E1-T1 | Schema v1 (manifesto) | todo | — | |
| M1-E1-T2 | Manifestos iniciais | todo | — | |
| M1-E1-T3 | Registry loader | todo | — | |
| M1-E1-T4 | Discover | todo | — | |
| M1-E2-T1 | Download engine | todo | — | |
| M1-E2-T2 | Verify + extract | todo | — | |
| M1-E2-T3 | Providers | todo | — | |
| M1-E2-T4 | Layout + current | todo | — | |
| M1-E2-T5 | Update engine | todo | — | |
| M1-E3-T1 | Job Objects + spawn + stop em cascata | todo | — | |
| M1-E3-T2 | Health probes + estados + restart + métricas | todo | — | |
| M1-E3-T3 | Pré-checagens | todo | — | |
| M1-E3-T4 | PhpPool | todo | — | |
| M1-E4-T1 | Templates de servidor web | todo | — | |
| M1-E4-T2 | Drivers de framework | todo | — | |
| M1-E4-T3 | DNS server + NRPT | todo | — | |
| M1-E4-T4 | TLS | todo | — | |
| M1-E4-T5 | Proxies + processes | todo | — | |
| M1-E4-T6 | Paridade Linux | todo | — | |
| M1-E5-T1 | Contratos IPC | review | feature/M1-E5-T1-ipc-contracts | ADR-0015. `forge-ipc`: DTOs por grupo do docs/09 + tabela única `METHODS` (macro → marcadores, `Client` tipado, `methods.ts`; teste cobre todo comando do docs/09), JSON-RPC 2.0/NDJSON com `request_id` obrigatório e cache de idempotência, `system.hello` + `state/ipc.token` (ACL via `FileAcl`), named pipe por raiz com DACL/dono = usuário (Win32 em `forge-core/platform/windows/pipe_security.rs`), unix socket mínimo (não compilado no CI Windows), in-memory, WebSocket placeholder; streams com fila drop-oldest + `*.lagged`, `resume_from` (anel do `EventBus` + tabela `events`), cliente com reconexão/reassinatura; `fs.*` restrito a etc/logs/www (canonicalize). forge-core: `EventBus::subscribe_from`, ids `SubscriptionId/SessionId/ClientId`, códigos `unauthorized/forbidden`, payloads job/actor com `specta::Type`. Pendente para M1-E5-T2: daemon registrar handlers, persistir eventos e servir no pipe. |
| M1-E5-T2 | Daemon | todo | — | |
| M1-E5-T3 | CLI completa | todo | — | |
| M1-E5-T4 | Helper elevado | todo | — | |
| M1-E5-T5 | Import do Laragon | todo | — | |
| M2-E1 | Shell do app (Tauri) | todo | — | |
| M2-E2 | Telas núcleo | todo | — | |
| M2-E3 | Telas complementares | todo | — | |
| M2-E4 | Design QA | todo | — | |
| M2-E5 | Métricas v1 | todo | — | |
| M3-E1 | Catálogo completo de manifestos | todo | — | |
| M3-E2 | Stacks + forge new + drivers restantes | todo | — | |
| M3-E3 | forge share, LAN, backups, bundles | todo | — | |
| M4-E1 | Instalador NSIS, portátil, desinstalador | todo | — | |
| M4-E2 | Documentação do usuário | todo | — | |
| M4-E3 | Beta fechado | todo | — | |

## Ferramentas instaladas na máquina de dev

| Ferramenta | Versão | Como foi instalada | Data |
|---|---|---|---|
| pnpm | 10.7.1 | scoop | 2026-10-26 |
| cargo-deny | 0.20.2 | `cargo install` | 2026-10-26 |
