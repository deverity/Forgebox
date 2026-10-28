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
| Sheet | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `sheet` | aberto, fechando | a fazer |
| Popover | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `popover` | aberto, fechando | a fazer |
| Toast | `bg-surface-2`, `shadow-popover` | `popover` | info, sucesso, erro | a fazer |

## Controles (M2-E1-T11)

Todos recebem textos já localizados do chamador (props `label`, `hint`, `emptyText`...); nenhum texto fixo, exceto o estado do StatusDot, localizado internamente em `common:status.*`. Nenhum aceita `className` (só tokens). Foco: anel `outline-accent` 2 px com offset 2 px (`FOCUS_RING`). Movimento: `useMotionPreset(preset)` (`design/useMotionPreset.ts`) reavalia `prefers-reduced-motion` ao vivo; sob movimento reduzido tudo vira fade de 100 ms (`data-motion="fade"`).

### Button / IconButton
- **Anatomia:** `<button>` 32 px de altura, borda 1 px, `radius-control`; rótulo em `span`; spinner sobreposto em `loading`. IconButton é 32x32, ícone 16 px e `label` obrigatório (`aria-label` + `title`).
- **Variantes:** `primary` (fundo `accent`, texto `fg-on-accent`), `secondary` (surface + `stroke-strong`), `ghost`, `destructive` (borda e texto `state-failed`).
- **Estados:** `loading` desabilita cliques, define `aria-busy` e mantém o rótulo no fluxo (invisível), então a largura não muda; `disabled` com opacidade 50 %.
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
- Radix `RadioGroup` (`role=radiogroup`, itens `role=radio`): setas movem e selecionam; Tab sai do grupo. A pílula de seleção desliza com `layoutId` e a mola `toggle`; reduced motion usa fade de 100 ms.

### Slider
- Radix `Slider` de um polegar (`role=slider`, `aria-valuemin/max/now`, `aria-valuetext` opcional). Setas = passo; PageUp/PageDown = 10 passos; Home/End = extremos.

### TextField
- `label` visível ligado por `htmlFor`; `hint` e erro ligados por `aria-describedby`; erro com `role=alert`, `aria-invalid` e borda `state-failed`.
- **Validação inline:** `validate(valor)` roda após o primeiro blur e depois a cada alteração (a mensagem some assim que o valor é corrigido); `error` força uma mensagem (ex.: do servidor) e vence `validate`.

### Badge
- Tons: `neutral`, `accent`, `success`, `warning`, `danger`, `info` (cor de borda e texto = token; o significado vai sempre no texto).

### StatusDot
- **Anatomia:** ponto 8 px `role=img` com `aria-label` = estado localizado (`common:status.*`); `showLabel` mostra o texto ao lado.
- **Animação (docs/07 §3):** `starting` e `updating` pulsam (escala + opacidade, 1 s, em loop); `running` respira (opacidade, 2 s, em loop); `stopped` estático; `failed` treme 1x (x ±2 px, 300 ms). Só `transform` e `opacity`.
- **Reduced motion:** todos os estados viram um único fade de opacidade de 100 ms ao mudar de estado (`data-animation="fade"`).

### Kbd
- `<kbd>` com borda `stroke-strong`, fonte mono 11 px.

## Testes
`src/design/primitives/*.test.tsx` (Vitest + Testing Library): papéis/aria, teclado (Espaço/Enter/setas/Escape), disabled/loading e reduced motion (`src/test/matchMedia.ts`). `src/app/gallery/Gallery.test.tsx` cobre a galeria e a rota.

## Modelo de cada seção

- **Anatomia:** partes e tokens.
- **Variantes e tamanhos.**
- **Estados** (tabela acima).
- **Acessibilidade:** papel ARIA (Radix), teclado, texto alternativo (estado nunca só por cor).
- **Motion:** preset de motion.md.
