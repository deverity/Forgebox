# Motion

Fonte: `tokens.json` (`motion.*`) e [docs/07 §3](../07-ui-ux.md). Código: `apps/desktop/src/design/motion.ts` (`resolveMotion(preset)`).
Biblioteca: Motion (framer-motion). Os presets têm o formato da prop `transition`.

## Springs

| Preset | stiffness | damping | Uso |
|---|---|---|---|
| `layout` | 380 | 32 | Layout e transições compartilhadas (`layoutId`) lista → detalhe |
| `popover` | 260 | 26 | Popovers, menus, tooltips |
| `toggle` | 500 | 40 | Toggles, switches, controles segmentados |

## Durações (tween `easeOut`)

| Preset | Duração | Uso |
|---|---|---|
| `micro` | 120 ms | Hover, press, foco, mudança de cor |
| `transition` | 220 ms | Troca de painel/rota, expandir/colapsar |
| `sheet` | 280 ms | Sheets e diálogos |

## Por interação

| Interação | Preset | Observação |
|---|---|---|
| Lista → detalhe de serviço | `layout` | `layoutId` compartilhado |
| Abrir popover/menu | `popover` | Origem no gatilho |
| Toggle on/off | `toggle` | Thumb por spring, cor em `micro` |
| Abrir/fechar sheet | `sheet` | Desliza + fade |
| Status `Starting` | pulso contínuo | Ponto de status |
| Status `Running` | "respira" 2 s | Sutil (opacidade) |
| Status `Stopped` | estático | |
| Status `Failed` | treme 1× | `layout` curto |
| Download concluído | `transition` | Barra faz morph para check |

## prefers-reduced-motion

Todo preset resolve para **fade de 100 ms** (`tween`, `linear`, `fadeOnly: true`): sem deslocamento, escala, pulso, "respiração" ou tremor; apenas opacidade. `resolveMotion` lê `matchMedia("(prefers-reduced-motion: reduce)")` na hora da chamada (não no import), então a mudança de preferência vale sem reload. Testado em `src/design/tokens.test.ts` com mock de `matchMedia`.

## Regras

- Nada de duração/spring literal fora de `tokens.json`.
- Animar só `transform` e `opacity`; nunca largura/altura (use `layout`).
- Nenhuma animação bloqueia interação (< 50 ms de resposta, docs/07 §10).
