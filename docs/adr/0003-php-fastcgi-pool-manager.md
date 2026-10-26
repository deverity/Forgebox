# ADR-0003 — Pool FastCGI próprio por versão de PHP

**Status:** aceito · **Data:** 2026-10-26

## Contexto
PHP no Windows não tem PHP-FPM. Opções existentes: `mod_php` (TS, uma versão por Apache), `mod_fcgid` (Apache spawna `php-cgi`, uma versão por vhost via `FcgidWrapper`, sem health/recycle controlados, problemas de latência intermitente relatados), ou N `php-cgi -b porta` lançados à mão (Laragon: porta fixa 10987, Herd: `$herd_sock` por site). Requisito central: **vários PHP ao mesmo tempo, o vhost escolhe, troca sem derrubar nada.**

## Decisão
`forge-fcgi`: para cada versão de PHP em uso, um pool de processos `php-cgi.exe`, **um por porta**, gerenciado pelo daemon (scaling min/max, recycle por requests/RSS/idade, warmup por FastCGI, health). Web servers balanceiam entre as portas (Nginx `upstream`, Apache `mod_proxy_fcgi` + `mod_proxy_balancer`, Caddy `php_fastcgi` com múltiplos backends). Troca de versão é blue/green: novo pool sobe, vhost re-renderizado, reload, pool antigo drenado.

## Alternativas rejeitadas
- **mod_fcgid:** amarra a Apache; sem controle fino; conflita com nosso supervisor.
- **mod_php:** uma versão por servidor; exige TS.
- **Vários processos na mesma porta (SO_REUSEADDR):** comportamento não garantido no Windows; sem visibilidade por worker.
- **Portar PHP-FPM para Windows:** fora de escopo.
- **FrankenPHP para tudo:** ótimo para Octane, mas muda semântica (worker mode, Caddy embutido); oferecido como opção por site, não como padrão.

## Consequências
- Precisamos de um cliente FastCGI mínimo (health/warmup) em Rust.
- Faixa de portas dedicada (19000–19999) e Xdebug 9003 reservada.
- Apache requer forma `SetHandler "proxy:balancer://..."` (não `ProxyPass`) para `SCRIPT_FILENAME` correto.
