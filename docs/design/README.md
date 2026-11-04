# docs/design

Artefatos visuais do design system definido em [07-ui-ux.md](../07-ui-ux.md).

Conteúdo esperado (a produzir em M2-E1 / M2-E4):

- `tokens.json` — **fonte única** (DTCG) dos tokens (cores light/dark, tipografia, espaçamentos, raios, sombras) consumido por `apps/desktop/src/design/tokens.css` via script.
- `screens/` — capturas de referência por tela, `<tela>-{light,dark}.png` com os ids da matriz de `qa:shots` (`onboarding`, `dashboard`, `services`, `sites`, `site-detail`, `php`, `databases`, `mail`, `logs`, `catalog`, `catalog-detail`, `terminal`, `settings`, `diagnostics`; M2-E4-T7). São as células núcleo a 100 % (1280x800, pt-BR, vibrancy ligado no navegador, cenário `populated`, `empty` no onboarding) de uma execução completa do `qa:shots`: `node apps/desktop/scripts/reference-shots.mjs --copy apps/desktop/qa/out/shots/<run>` copia e valida; sem `--copy` só valida (PNG 1280x800, <= 250 KB cada, <= 8 MB no total, as duas variantes de cada tela), e `src/qa/referenceShots.test.ts` repete a validação no `pnpm test`. A bandeja (`tray`) não está aqui: a referência dela é `brand/reference-icon-tray.png`. O quadro nativo (material, bolinhas) não aparece numa captura de navegador; para ele use `scripts/qa/capture-window.ps1`.
- `components.md` — anatomia e estados de cada primitive (Button, Toggle, StatusDot, Sheet, Toast…).
- `motion.md` — tabela de springs/durações por interação, com GIFs de referência.
- `brand/` — ícone do Forgebox (app/EXEs) e ícones de estado da bandeja: arte de referência do usuário, master SVG, grades pixel a pixel de 16–32 px, paleta, folha de contato; ver [brand/README.md](brand/README.md) (M2-E1-T22).
- `icons/` — ícones de tools em SVG (mono + color), 24 px grid, nomes = `metadata.id` do manifesto.
- `reference/glass-mockup.html` — mockup aprovado pelo usuário em 2026-11-04 da moldura de vidro (sidebar em altura total, bolinhas da janela à direita na ordem do Windows, visual mais leve); fonte da M2-E1-T23, abra no navegador (claro/escuro/janela sem foco).

Regra: nenhum valor visual novo entra em código sem estar aqui primeiro.
