# forge-sites

**O que é:** Modelo de site (validação de host/aliases, `SiteDraft`, `SiteService` com eventos `site.*`), drivers de framework (Laravel, Symfony, WordPress, CakePHP, Drupal, Magento, generic-php, static, spa), conversão `Site` → `VhostModel`, parked dirs (watcher `notify`) e o loader de `forgebox.yaml` (`ProjectPlan`). Proxies e apps (Procfile) chegam na M1-E4-T5.

**O que não é:** Não renderiza arquivos de configuração (isso é `forge-render`) e não mexe em DNS nem em certificados.

**Quem depende dele:** `forge-daemon`.

**Referência:** [docs/06-sites-vhosts-multi-php.md](../../docs/06-sites-vhosts-multi-php.md) (§1.3 descreve a implementação).

**Testes:** fixtures em `tests/fixtures/projects/<nome>/` (só arquivos marcadores) e snapshots `insta` em `tests/snapshots/` (`INSTA_UPDATE=always cargo test -p forge-sites` para atualizar).
