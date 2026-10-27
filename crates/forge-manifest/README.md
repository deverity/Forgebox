# forge-manifest

**O que é:** Schema v1 do manifesto `forgebox.yaml` (serde + schemars) para os kinds `Tool`, `Extension`, `Stack`, `Project` e `Registry`; parser com erros de linha/coluna; validação semântica; regras de versão; lint das 7 regras de [docs/04](../../docs/04-tool-manifest-spec.md) §6.

**O que não é:** Não baixa nada e não lê/assina o índice remoto (isso é `forge-registry`). Não renderiza templates (`forge-render`). Não tem lógica específica de tool: tudo vem dos manifestos.

**Quem depende dele:** `forge-registry`, `forge-daemon` e `xtask` (`schema`, `lint-registry`).

**API:**
- `load_str(&str) -> Result<Manifest, ManifestError>` (lê `apiVersion`/`kind` e parseia direto no struct do kind; `deny_unknown_fields` em todos os níveis), `load_file`, `parse::to_yaml`.
- `validate(&Manifest)` / `validate_with(&Manifest, &ValidateOptions{port_ranges, third_party})` → `Vec<ValidationIssue{code, field, message}>`; `IssueCode::as_str()` é estável (chave de i18n) e `lint_rule()` diz a regra (1..7).
- `version`: `Version::parse(scheme, ..)`, `Constraint::parse(scheme, ..)` (`>=7.4 <9`, `~8.3`, `3.4.*`, `||`; versão sem operador = exata), `channel_matches`, `series_key`, `canonical_name` (`8.3.37-nts-vs17-x64`), `cmp_builds`/`sort_builds`.
- `lint::lint_registry(&RegistryDir, &LintContext{known_templates, template_roots, port_ranges}) -> LintReport` (I/O bloqueante: `spawn_blocking`).
- `schema::json_schemas()` → `registry/schema/{tool,extension,stack,project,registry}.v1.json`.

**Decisões:** cada kind tem `kind` explícito (enum de um valor) para o JSON Schema descrever o documento inteiro e para o parse direto manter a posição dos erros; `Manifest` despacha por `kind` com `Deserialize` próprio (o `#[serde(tag)]` do serde bufferiza o documento e perde linha/coluna). Ações (`post_install`...) são mapas de uma chave (`yaml.rs` usa `singleton_map_recursive`).

**Testes:** fixtures em `tests/fixtures/{valid,invalid}` (cada inválido declara `# expect: <códigos>`), snapshots `insta` dos JSON Schemas em `tests/snapshots/`.

**Referência:** [docs/04](../../docs/04-tool-manifest-spec.md), [docs/05](../../docs/05-providers-and-updates.md) §1, [docs/08](../../docs/08-release-layout.md) §5, [ADR-0006](../../docs/adr/0006-yaml-manifests-registry.md), [ADR-0012](../../docs/adr/0012-yaml-crate-serde-yaml-ng.md).
