# docs/design

Artefatos visuais do design system definido em [07-ui-ux.md](../07-ui-ux.md).

Conteúdo esperado (a produzir em M2-E1 / M2-E4):

- `tokens.json` — export dos tokens (cores light/dark, tipografia, espaçamentos, raios, sombras) consumido por `apps/desktop/src/design/tokens.css` via script.
- `screens/` — wireframes/mockups por tela (`onboarding.png`, `dashboard.png`, `services.png`, `services-detail.png`, `sites.png`, `site-detail.png`, `php.png`, `databases.png`, `mail.png`, `logs.png`, `catalog.png`, `catalog-tool.png`, `terminal.png`, `settings.png`, `tray.png`), em light e dark.
- `components.md` — anatomia e estados de cada primitive (Button, Toggle, StatusDot, Sheet, Toast…).
- `motion.md` — tabela de springs/durações por interação, com GIFs de referência.
- `icons/` — ícones de tools em SVG (mono + color), 24 px grid, nomes = `metadata.id` do manifesto.

Regra: nenhum valor visual novo entra em código sem estar aqui primeiro.
