# xtask

**O que é:** Tarefas de desenvolvimento do Forgebox, executadas com `cargo xtask <comando>` (alias em `.cargo/config.toml`).

**O que não é:** Não é o pipeline de release nem o instalador; esses entram em M4-E1.

**Comandos:**
- `schema`: grava `registry/schema/{tool,extension,stack,project,registry}.v1.json` a partir de `forge_manifest::schema::json_schemas()` (schemars).
- `gen-ts`: grava `apps/desktop/src/ipc/generated.ts` a partir de `forge_ipc::api::contracts::export_typescript()` (specta + specta-typescript + specta-serde).
- `lint-registry`: roda `forge_manifest::lint::lint_registry` (as 7 regras de docs/04 §6) sobre `registry/` (`tools/*.yaml`, `stacks/*.yaml`, `index.yaml`, `icons/`), com `LintContext` montado a partir de `crates/forge-render/templates/` (templates embutidos) e das faixas de porta padrão de `forge_core::config`; imprime `arquivo:linha:coluna: [rule N código] campo: mensagem` e sai com 1 se houver issues. Sem `registry/`, imprime "registry/ not found, nothing to lint" e sai 0.
- `qa-fixtures [--out <dir>] [--only not-implemented]` (M2-E4-T1): sobe um `forgeboxd` real em raiz temporária com binários falsos (`fake_php`), chama só métodos Viewer não mutantes (`daemon.status`, `services.status/ports`, `sites.list/info`, `php.list/pool_status`, `tools.list/search/info`, `net.dns_status`, `tls.ca_status/list`, `jobs.list`, `events.list`, `logs.read`; afirmado contra `METHODS`) e grava JSON normalizado (ULIDs e horários em marcadores estáveis, raiz `C:/Forgebox`) em `apps/desktop/qa/fixtures/recorded/`, mais `not_implemented.json` a partir de `NOT_IMPLEMENTED`. `--only not-implemented` grava só esse arquivo, sem daemon: é o subconjunto determinístico que o CI confere com `git diff --exit-code`; o vitest `recordedFixtures.test.ts` exige que a lista do fakeBridge seja igual. Os demais arquivos dependem da máquina e são regravados à mão.
- `ci`: fmt-check, clippy `-D warnings`, test, `cargo deny check` e lint-registry, parando na primeira falha.

**Estado:** implementado em M0-E1-T2; schemas completos e lint com as 7 regras em M1-E1-T1. Os contratos TS ainda são mínimos (expandem em M1-E5-T1). Os artefatos gerados são versionados; o CI deve rodar `schema` e `gen-ts` e falhar se `git diff` não estiver vazio.

**Referência:** [docs/10-roadmap-and-agent-tasks.md](../docs/10-roadmap-and-agent-tasks.md) (M0-E1-T2).
