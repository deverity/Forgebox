# Componentes (esqueleto)

Anatomia e estados de cada primitive de `apps/desktop/src/design/primitives/` ([docs/07 §6](../07-ui-ux.md)). Cada seção é preenchida pela tarefa que implementa o primitive (M2-E1-T9 em diante) e visível em `/__gallery`.

Regras comuns: só tokens (`bg-surface`, `text-fg-primary`, `rounded-control`, `shadow-popover`); texto mínimo 11 px; foco visível com anel `accent`; estados `default`, `hover`, `pressed`, `focus-visible`, `disabled`, `loading`; light, dark, vibrancy off e alto contraste.

| Primitive | Tokens principais | Motion | Estados | Status |
|---|---|---|---|---|
| Button | `accent`, `radius-control`, `text-body` | `micro` | default, hover, pressed, focus, disabled, loading | a fazer |
| Toggle | `accent`, `stroke-strong` | `toggle` | on, off, focus, disabled | a fazer |
| StatusDot | `state-*` | pulso / respiração (ver motion.md) | running, starting, stopped, failed, updating | a fazer |
| Sheet | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `sheet` | aberto, fechando | pronto (M2-E1-T12) |
| Popover | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `popover` | aberto, fechando | pronto (M2-E1-T12) |
| Toast | `bg-surface-2`, `shadow-popover` | `popover` | info, sucesso, erro | pronto (M2-E1-T12) |
| Card | `bg-surface`, `stroke`, `radius-card` | — | default | pronto (M2-E1-T12) |
| ListRow | `bg-surface-2`, `fg-secondary`, `radius-control` | `micro` (hover) | default, hover, selected, focus | pronto (M2-E1-T12) |
| VirtualList | — | — | vazio, rolando | pronto (M2-E1-T12); medição de fps no M2-E4-T4 |
| Menu | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `popover` | aberto, fechando, item desabilitado | pronto (M2-E1-T12) |
| Tooltip | `bg-surface-2`, `shadow-popover` | `popover` | aberto, fechando | pronto (M2-E1-T12) |
| Progress | `accent`, `state-running`, `stroke` | `transition` | andamento, indeterminado, concluído | pronto (M2-E1-T12) |
| Sparkline | `accent`, `fg-secondary`, `tnum` | — | com dados, vazio | pronto (M2-E1-T12) |
| EmptyState | `fg-tertiary`, `text-headline` | — | default | pronto (M2-E1-T12) |
| Skeleton | `stroke`, `radius-control` | pulso CSS (só `motion-safe`) | carregando | pronto (M2-E1-T12) |

## Modelo de cada seção

- **Anatomia:** partes e tokens.
- **Variantes e tamanhos.**
- **Estados** (tabela acima).
- **Acessibilidade:** papel ARIA (Radix), teclado, texto alternativo (estado nunca só por cor).
- **Motion:** preset de motion.md.

## Superfícies e feedback (M2-E1-T12)

Código em `apps/desktop/src/design/primitives/`; galeria em `src/design/gallery/SurfacesGallery.tsx` (a rota `/__gallery` do M2-E1-T11 monta essa seção). Importe de `design/primitives` (`index.ts`).

Movimento: `Sheet`, `Popover`, `Menu`, `Tooltip` e `Toast` usam `surfaceMotion(kind)` (`useSurfaceMotion.ts`), que lê `prefers-reduced-motion` a cada render e, quando ativo, troca tudo por fade de 100 ms só com opacidade. O elemento animado expõe `data-motion="full|reduced"`. Só `transform` e `opacity` animam.

### Card
- **Anatomia:** `div` com `border border-stroke bg-surface rounded-card`; sem sombra (docs/07 §2.5). `padding`: `none | md (16) | lg (24)`.

### ListRow
- **Anatomia:** altura fixa de 48 px; slot `leading` (20 px), título + subtítulo (truncados), slot `trailing`.
- **Acessibilidade:** com `onClick` vira `<button>` (`aria-pressed` = selecionado), anel de foco `accent`; sem `onClick` é estático.

### VirtualList
- `@tanstack/react-virtual`, altura de linha fixa (padrão 48), `overscan` 8, chave estável via `getKey`. Só as linhas visíveis ficam no DOM (500 itens montam ~20). `role="list"`/`listitem` com `aria-posinset`/`aria-setsize`. O contêiner precisa ter altura limitada. Teste mede a janela montada; o fps real é medido no M2-E4-T4.

### Sheet
- **Anatomia:** Radix Dialog, painel lateral de 420 px à direita, cabeçalho (título + descrição + fechar), corpo rolável, scrim.
- **Motion:** `sheet` (280 ms, desliza + fade); reduzido: fade de 100 ms.
- **Acessibilidade:** foco preso, Escape fecha e devolve o foco ao gatilho, `aria-labelledby`/`describedby` pelo Radix.

### Popover
- **Anatomia:** Radix Popover **modal** (foco preso), raio 12 (`rounded-sheet`), `shadow-popover`. `label` vira o nome acessível.
- **Motion:** spring `popover` 260/26 com origem no gatilho; reduzido: fade de 100 ms. Escape fecha e devolve o foco.

### Menu
- **Anatomia:** Radix ContextMenu; abre com botão direito, toque longo ou tecla de menu de contexto. Itens com atalho, separador, desabilitado e destrutivo (`state-failed`). A área-gatilho é focusable (`tabIndex=0`) para o foco voltar a ela.
- **Acessibilidade:** setas, typeahead, Enter, Escape. **Motion:** `popover`.

### Toast
- **API:** `toast.show({ kind, message, undo?, durationMs? })`, `toast.dismiss(id)`; montar `<ToastViewport/>` uma vez na raiz. Pilha de até 4 (os mais antigos saem); erros não somem sozinhos; hover/foco pausa o timer.
- **Acessibilidade:** duas regiões vivas sempre no DOM: `role="status"` polite (info/sucesso) e `role="alert"` assertive (erro). Ação "Desfazer" é um botão real.
- **Motion:** `popover`; reduzido: fade de 100 ms.

### Tooltip
- Radix Tooltip (atraso 400 ms). Slot `command` mostra o comando `forge` equivalente em mono (`data-slot="command"`, nunca traduzido; docs/07 §11).

### Progress
- **Variantes:** `linear` e `circular`; `value` 0-100 ou `null` (indeterminado). Texto: `42% · 12 MB/s · faltam 1m 15s` (formatação por locale, `tabular-nums`).
- **Concluído (100 ou `done`):** a barra/anel vira `state-running` e um check é desenhado (morph, `transition` 220 ms; reduzido: só fade); o texto vira "Concluído". `role="progressbar"` com `aria-valuenow`/`aria-valuetext`.

### Sparkline
- SVG de até 60 amostras (`SPARKLINE_SAMPLES`), rótulos atual/pico em `tabular-nums`, `role="img"` com resumo. `React.memo` compara **valores** das amostras: re-render do pai ou de um irmão com dados iguais não re-renderiza (teste conta renders).

### EmptyState
- Slot `illustration` (decorativo, `aria-hidden`), título, descrição e slot `action` (botão primário).

### Skeleton
- `Skeleton` tem altura fixa obrigatória; `SkeletonSlot` reserva a altura nos dois estados (`aria-busy`), então carregar → conteúdo não muda o layout. Pulso CSS só com `motion-safe`.
