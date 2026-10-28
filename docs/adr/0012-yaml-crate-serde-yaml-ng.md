# ADR-0012 — YAML com `serde_yaml_ng`

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Manifestos (`forgebox.yaml`, [04](../04-tool-manifest-spec.md)) e configs do usuário em YAML precisam de serde, bom suporte a schemas (`schemars`) e erros com linha/coluna. O `serde_yaml` original foi arquivado em 2024. O `serde_yml`, indicado inicialmente em [01](../01-architecture.md) e no CLAUDE.md, foi deprecado em 2026 e passou a redirecionar para `noyalib`. Manter um crate deprecado no núcleo do produto é risco de segurança e de manutenção.

## Decisão
Usar **`serde_yaml_ng`** (fork mantido do `serde_yaml`) como implementação de YAML para manifestos e configs. O acesso ao YAML fica atrás de um módulo `forge_manifest::yaml`, para que a troca de crate seja localizada em um único lugar.

## Alternativas rejeitadas
- **`serde_yml`:** deprecado em 2026 e redirecionando para outro crate.
- **`serde-saphyr` / `serde_yaml_bw`:** API diferente e menos adoção. Reavaliar se precisarmos de posição de erro mais rica do que a que `serde_yaml_ng` oferece.
- **`noyalib`:** jovem demais para o núcleo; reavaliar quando tiver histórico de manutenção.

## Consequências
- `schemars` continua funcionando, pois a integração é via serde.
- Erros de parse trazem linha e coluna, suficientes para a validação de manifestos (M1-E1-T1).
- Qualquer troca futura fica restrita ao módulo `forge_manifest::yaml`; isso exige um novo ADR que substitua este.
