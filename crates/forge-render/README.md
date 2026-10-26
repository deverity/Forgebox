# forge-render

**O que é:** Renderização de configs de servidores e tools (httpd, nginx, php.ini, my.ini, postgresql.conf) com minijinja, blocos `# forgebox:begin/end` e detecção de drift.

**O que não é:** Não escreve fora dos blocos gerenciados e não decide regras de site (isso é `forge-sites`).

**Quem depende dele:** `forge-sites` e `forge-daemon`.

**Referência:** [docs/01-architecture.md](../../docs/01-architecture.md) §3, [docs/14-observability-metrics.md](../../docs/14-observability-metrics.md) §2 e [docs/15-linux-parity-case-sensitivity.md](../../docs/15-linux-parity-case-sensitivity.md) §4.
