# forge-fcgi

**O que é:** Gerenciador de pools FastCGI de PHP (`php-cgi.exe`) por versão: portas, workers, recycle, warmup e troca blue/green.

**O que não é:** Não gera vhosts nem configs de servidor web (isso é `forge-render` e `forge-sites`).

**Quem depende dele:** somente `forge-daemon`.

**Referência:** [docs/03-engine.md](../../docs/03-engine.md) §2, [docs/06-sites-vhosts-multi-php.md](../../docs/06-sites-vhosts-multi-php.md) §2 e [ADR-0003](../../docs/adr/0003-php-fastcgi-pool-manager.md).
