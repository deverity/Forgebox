# forge-manifest

**O que é:** Schema do manifesto `forgebox.yaml` (serde + schemars), parser, validação semântica e regras de versão.

**O que não é:** Não baixa nada e não lê o índice do registry (isso é `forge-registry`). Não tem lógica específica de tool.

**Quem depende dele:** `forge-registry` e `forge-daemon`.

**Referência:** [docs/04-tool-manifest-spec.md](../../docs/04-tool-manifest-spec.md) e [ADR-0006](../../docs/adr/0006-yaml-manifests-registry.md).
