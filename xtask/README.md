# xtask

**O que é:** Tarefas de desenvolvimento do Forgebox, executadas com `cargo xtask <comando>` (alias em `.cargo/config.toml`).

**O que não é:** Não é o pipeline de release nem o instalador; esses entram em M4-E1.

**Comandos:**
- `schema`: grava `registry/schema/{tool,stack,project}.v1.json` a partir de `forge_manifest::schema::json_schemas()` (schemars).
- `gen-ts`: grava `apps/desktop/src/ipc/generated.ts` a partir de `forge_ipc::api::contracts::export_typescript()` (specta + specta-typescript + specta-serde).
- `lint-registry`: roda `forge_manifest::lint::lint_file` em `registry/tools/*.yaml`, `registry/stacks/*.yaml` e `registry/index.yaml`; sai com 1 se houver issues. Sem `registry/`, imprime "registry/ not found, nothing to lint" e sai 0.
- `ci`: fmt-check, clippy `-D warnings`, test, `cargo deny check` e lint-registry, parando na primeira falha.

**Estado:** implementado em M0-E1-T2. Os schemas e os contratos TS ainda são mínimos (expandem em M1-E1-T1 e M1-E5-T1). Os artefatos gerados são versionados; o CI deve rodar `schema` e `gen-ts` e falhar se `git diff` não estiver vazio.

**Referência:** [docs/10-roadmap-and-agent-tasks.md](../docs/10-roadmap-and-agent-tasks.md) (M0-E1-T2).
