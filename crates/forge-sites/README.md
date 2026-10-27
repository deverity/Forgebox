# forge-sites

**O que é:** Modelo de site (validação de host/aliases, `SiteDraft`, `SiteService` com eventos `site.*`), drivers de framework (Laravel, Symfony, WordPress, CakePHP, Drupal, Magento, generic-php, static, spa), conversão `Site` → `VhostModel`, parked dirs (watcher `notify`) o loader de `forgebox.yaml` (`ProjectPlan`) e a paridade Linux dos projetos ([docs/15](../../docs/15-linux-parity-case-sensitivity.md), [ADR-0024](../../docs/adr/0024-case-conversion-tree-recreation-and-mismatch-finder.md)): Case Mismatch Finder (`lint_case`: referências com caixa diferente do disco em PHP/Blade/Twig/HTML/JS/CSS/manifestos/PSR-4, nomes duplicados por caixa, nomes reservados do Windows, correção por renomear/editar com prévia), estado `Aa`/`aA` por site com re-verificação no boot e jobs (`case_parity`) e paridade Git (`parity::git_parity_init`). Proxies e apps (Procfile) chegam na M1-E4-T5.

**O que não é:** Não renderiza arquivos de configuração (isso é `forge-render`) e não mexe em DNS nem em certificados.

**Quem depende dele:** `forge-daemon`.

**Referência:** [docs/06-sites-vhosts-multi-php.md](../../docs/06-sites-vhosts-multi-php.md) (§1.3 descreve a implementação).

**Testes:** fixtures em `tests/fixtures/projects/<nome>/` (só arquivos marcadores) e `tests/fixtures/case/` (aceite do Finder: 3 referências com caixa errada; os testes copiam para uma pasta temporária antes de corrigir) e snapshots `insta` em `tests/snapshots/` (`INSTA_UPDATE=always cargo test -p forge-sites` para atualizar).
