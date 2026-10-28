# forge-registry

**O que é:** Índice de manifestos (fontes git, http e pasta local), cache em `state/registry/`, busca e discovery de pastas em `bin/`.

**O que não é:** Não baixa binários de tools (isso é `forge-provider`) e não reimplementa o schema (usa `forge-manifest`).

**Quem depende dele:** `forge-provider` e `forge-daemon`.

**Referência:** [docs/04-tool-manifest-spec.md](../../docs/04-tool-manifest-spec.md), [docs/05-providers-and-updates.md](../../docs/05-providers-and-updates.md) e [docs/02-project-structure.md](../../docs/02-project-structure.md).
