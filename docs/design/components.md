# Componentes (esqueleto)

Anatomia e estados de cada primitive de `apps/desktop/src/design/primitives/` ([docs/07 §6](../07-ui-ux.md)). Cada seção é preenchida pela tarefa que implementa o primitive (M2-E1-T9 em diante) e visível em `/__gallery`.

Regras comuns: só tokens (`bg-surface`, `text-fg-primary`, `rounded-control`, `shadow-popover`); texto mínimo 11 px; foco visível com anel `accent`; estados `default`, `hover`, `pressed`, `focus-visible`, `disabled`, `loading`; light, dark, vibrancy off e alto contraste.

| Primitive | Tokens principais | Motion | Estados | Status |
|---|---|---|---|---|
| Button | `accent`, `radius-control`, `text-body` | `micro` | default, hover, pressed, focus, disabled, loading | a fazer |
| Toggle | `accent`, `stroke-strong` | `toggle` | on, off, focus, disabled | a fazer |
| StatusDot | `state-*` | pulso / respiração (ver motion.md) | running, starting, stopped, failed, updating | a fazer |
| Sheet | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `sheet` | aberto, fechando | a fazer |
| Popover | `bg-surface-2`, `shadow-popover`, `radius-sheet` | `popover` | aberto, fechando | a fazer |
| Toast | `bg-surface-2`, `shadow-popover` | `popover` | info, sucesso, erro | a fazer |

## Modelo de cada seção

- **Anatomia:** partes e tokens.
- **Variantes e tamanhos.**
- **Estados** (tabela acima).
- **Acessibilidade:** papel ARIA (Radix), teclado, texto alternativo (estado nunca só por cor).
- **Motion:** preset de motion.md.
