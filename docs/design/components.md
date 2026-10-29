# Componentes (esqueleto)

Anatomia e estados de cada primitive de `apps/desktop/src/design/primitives/` ([docs/07 §6](../07-ui-ux.md)). Cada seção é preenchida pela tarefa que implementa o primitive e visível em `/__gallery` (rota de dev/QA, carregada sob demanda; arquivo `apps/desktop/src/app/gallery/Gallery.tsx`).

Regras comuns: só tokens (`bg-surface`, `text-fg-primary`, `rounded-control`, `shadow-popover`); texto mínimo 11 px; foco visível com anel `accent`; estados `default`, `hover`, `pressed`, `focus-visible`, `disabled`, `loading`; light, dark, vibrancy off e alto contraste.

| Primitive | Tokens principais | Motion | Estados | Status |
|---|---|---|---|---|
| Button | `accent`, `fg-on-accent`, `radius-control`, `text-body` | `micro` (cor) | default, hover, pressed, focus, disabled, loading | feito (T11) |
| IconButton | idem Button | `micro` | default, hover, focus, disabled, loading | feito (T11) |
| Toggle | `accent`, `stroke-strong` | `toggle` (mola 500/40) | on, off, focus, disabled | feito (T11) |
| Select | `bg-surface-2`, `shadow-popover`, `radius-sheet` | n/a | fechado, aberto, item destacado, disabled | feito (T11) |
| Combobox | idem Select | n/a | fechado, aberto, filtrando, vazio, disabled | feito (T11) |
| SegmentedControl | `bg-surface`, `stroke-strong` | `toggle` (pílula) | selecionado, focus, disabled | feito (T11) |
| Slider | `accent`, `stroke-strong` | n/a | default, focus, disabled | feito (T11) |
| TextField | `stroke-strong`, `state-failed` | n/a | default, focus, erro, disabled | feito (T11) |
| Badge | `state-*`, `accent` | n/a | 6 tons | feito (T11) |
| StatusDot | `state-*` | pulso / respiração / tremor (ver motion.md) | running, starting, stopped, failed, updating | feito (T11) |
| Kbd | `stroke-strong`, `bg-surface-2` | n/a | n/a | feito (T11) |
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

## Controles (M2-E1-T11)

Todos recebem textos já localizados do chamador (props `label`, `hint`, `emptyText`...); nenhum texto fixo, exceto o estado do StatusDot, localizado internamente em `common:status.*`. Nenhum aceita `className` (só tokens). Foco: anel `outline-accent` 2 px com offset 2 px (`FOCUS_RING`; no Tailwind 4 `outline-hidden` zera o estilo, por isso `focus-visible:outline-solid` e obrigatório, com teste de regressão). Alto contraste (forced-colors): Toggle, Slider e StatusDot usam `forced-color-adjust-none` para pintar o estado com os valores `Highlight`/`CanvasText` de `tokens.css`. Movimento: `useMotionPreset(preset)` (`design/useMotionPreset.ts`) reavalia `prefers-reduced-motion` ao vivo; sob movimento reduzido tudo vira fade de 100 ms (`data-motion="fade"`).

### Button / IconButton
- **Anatomia:** `<button>` 32 px de altura, borda 1 px, `radius-control`; conteúdo em grade de uma célula (`grid` + `col-start-1 row-start-1`) com dois blocos empilhados: o normal e o de loading (spinner inline de 16 px à esquerda + texto). IconButton é 32x32, ícone 16 px e `label` obrigatório (`aria-label` + `title`).
- **Variantes:** `primary` (fundo `accent`, texto `fg-on-accent`), `secondary` (surface + `stroke-strong`), `ghost`, `destructive` (borda e texto `state-failed`).
- **Estados:** `loading` desabilita cliques, define `aria-busy` e mostra spinner + texto (nunca spinner sozinho, docs/07 §1.2): `loadingLabel` (já localizado pelo chamador) troca o texto, senão o rótulo normal continua visível. O bloco inativo fica `opacity-0` + `aria-hidden`, então o nome acessível é igual ao texto visível e a largura é o máximo dos dois (sem layout shift). Em `loading` o cursor é `progress` e a opacidade 100 %; `disabled` (sem loading) usa opacidade 50 % e `not-allowed`.
- **Teclado:** Enter e Espaço (nativo).

### Toggle
- **Anatomia:** Radix `Switch` (`role=switch`, `aria-checked`), trilho 36x20, thumb 16 px que anda 16 px por mola `toggle` (500/40); cor do trilho em `micro`. Controlado (`checked`) ou não (`defaultChecked`).
- **Reduced motion:** o thumb salta e só a cor faz fade de `--duration-reduced` (100 ms).
- **Teclado:** Espaço alterna.

### Select
- Radix `Select`: gatilho `role=combobox`, lista `role=listbox` num portal. Enter/Espaço/setas abrem; setas/Home/End/typeahead navegam; Enter escolhe; Escape fecha. Itens `disabled` suportados.

### Combobox
- Radix não tem combobox; implementação WAI-ARIA editável com autocomplete de lista: `input role=combobox` + `aria-expanded/controls/activedescendant/autocomplete=list`, `ul role=listbox` com `role=option` e `aria-selected`.
- **Teclado:** ArrowDown/Up abrem e movem (circular); Enter escolhe; Escape fecha e descarta o filtro; digitar filtra por substring sem diferenciar maiúsculas; sem resultado mostra `emptyText`.

### SegmentedControl
- Radix `RadioGroup` (`role=radiogroup`, itens `role=radio`): setas movem e selecionam; Tab sai do grupo. A pílula de seleção desliza com `layoutId` e a mola `toggle`; em reduced motion a pílula é renderizada sem `layoutId` (que deslizaria) e faz fade de opacidade 0 para 1 com o preset reduzido (100 ms).

### Slider
- Radix `Slider` de um polegar (`role=slider`, `aria-valuemin/max/now`, `aria-valuetext` opcional). Setas = passo; PageUp/PageDown = 10 passos; Home/End = extremos.

### TextField
- `label` visível ligado por `htmlFor`; `hint` e erro ligados por `aria-describedby`; erro com `role=alert`, `aria-invalid` e borda `state-failed`. A cor da borda é escolhida em exatamente uma classe (`message ? border-state-failed : border-stroke-strong`): duas cores no mesmo elemento são resolvidas pela ordem do CSS do Tailwind, não pela do `className` (`FIELD_BASE` não tem cor de borda; `conflictingColorClasses` em `shared.ts` + teste guardam isso).
- **Largura:** prop `width` (`sm` 160 px, `md` 256 px, padrão, `full`), também em Select, Combobox e Slider; os primitives não aceitam `className`.
- **Validação inline:** `validate(valor)` roda após o primeiro blur e depois a cada alteração (a mensagem some assim que o valor é corrigido); `error` força uma mensagem (ex.: do servidor) e vence `validate`.

### Badge
- Tons: `neutral`, `accent`, `success`, `warning`, `danger`, `info` (o tom vai na borda; o texto é `fg-primary`/`fg-secondary`, porque os `state-*` garantem só 3:1 e texto pequeno exige 4.5:1, docs/07 §2.3; o significado vai sempre no texto).

### StatusDot
- **Anatomia:** ponto 8 px `role=img` com `aria-label` = estado localizado (`common:status.*`); `showLabel` mostra o texto ao lado e esconde o ponto da árvore de acessibilidade (`aria-hidden`, sem `role=img`) para o estado não ser lido duas vezes.
- **Animação (docs/07 §3):** `starting` e `updating` pulsam (escala + opacidade, 1 s, em loop); `running` respira (opacidade, 2 s, em loop); `stopped` estático; `failed` treme 1x (x ±2 px, 300 ms); durações e amplitudes na tabela "StatusDot" de motion.md. Só `transform` e `opacity`.
- **Reduced motion:** todos os estados viram um único fade de opacidade de 100 ms ao mudar de estado (`data-animation="fade"`).

### Kbd
- `<kbd>` com borda `stroke-strong`, fonte mono 11 px.

## Testes
`src/design/primitives/*.test.tsx` (Vitest + Testing Library): papéis/aria, teclado (Espaço/Enter/setas/Escape), disabled/loading e reduced motion (`src/test/matchMedia.ts`, único helper `mockReducedMotion`, restaurado sozinho após cada teste). `src/app/gallery/Gallery.test.tsx` cobre a galeria (inclui a seção `SurfacesGallery` do T12) e a rota. Os primitives são exportados por `design/primitives/index.ts`; a galeria importa dele.

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
