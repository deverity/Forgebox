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
| M1-E1-T1 | Schema v1 (manifesto) | done | feature/M1-E1-T1-manifest-schema-v1 | Schema v1 dos 5 kinds (`deny_unknown_fields` em todos os níveis, `kind` explícito por kind), `load_str` com linha/coluna, `validate`/`validate_with`, `version.rs` (semver tolerante, calver/string por segmentos, `||`, canais com prerelease, `canonical_name`, ordenação de builds), `lint_registry` com as 7 regras (`LintContext` montado pelo xtask a partir de `crates/forge-render/templates/`). 6 fixtures válidos + 27 inválidos (`# expect:`), snapshots insta dos 5 JSON Schemas, teste que valida os exemplos de docs/04. docs/04 ajustado: `service.daemon` aninhado, `compat[].tool`, `archive: file`, `post_create: - run: {command, cwd}`, variante `arch`, tabela de padrões, regra 2 estendida a providers sem checksum upstream. forge-core ganhou feature `schemars` (JsonSchema de `ToolId`). Deps novas: humantime, semver, regex; insta (dev). |
| M1-E1-T2 | Manifestos iniciais | review | feature/M1-E1-T2-initial-manifests | 12 tools/extensions + 4 stacks em `registry/` (lint 0 issues), ícones SVG genéricos, templates mínimos (`mysql`, `mariadb`, `php`, `postgresql`), `cargo xtask registry-index` + `index.yaml` gerado (lint confere sha256; assinatura `dev` com `sig` vazio só para `key_id: dev`), `tests/registry_lint.rs`. Hashes reais conferidos (PHP, Apache Lounge, Caddy sha512, Composer, Node, MariaDB API, Memurai via winget, Xdebug x5; nginx/MySQL/EDB/Mailpit curados, sem fonte upstream). **Validação empírica:** MySQL 8.4.13 e MariaDB 11.8.9 iniciam com `lower_case_table_names=0` em NTFS-CS (PASS), ver docs/15 §5; `=0` sem a flag aborta nos dois, `=2` em dir CS é forçado a 0. Achado: marcar a flag exige `FILE_DELETE_CHILD` no diretório (docs/15 §3). Memurai: MSI via `msiexec /a` (M1-E2-T2). Templates de servidor web/Caddyfile ficam para M1-E4-T1. |
| M1-E1-T3 | Registry loader | todo | — | |
| M1-E1-T4 | Discover | todo | — | |
| M1-E2-T1 | Download engine | done | feature/M1-E2-T1-download-engine | `forge-provider::download` (DownloadManager = ator de fila + jobs), 25 testes de integração com axum local (Range, corte+retomada, mirrors, cancelamento, progresso, prioridade, 2 paralelos, proxy manual, offline, banda). Deps novas: reqwest 0.13 (rustls, aws-lc-rs), sha2, url, axum (dev). `JobProgress` ganhou `speed_bps`/`eta_secs`. Docs 02/03/05 atualizados. |
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
| M1-E5-T1 | Contratos IPC | todo | — | |
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
