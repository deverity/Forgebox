# forge-net

**O que é:** Alocação de portas, servidor DNS para `*.test` (hickory), política NRPT e hosts via helper, e regras de firewall.

**O que não é:** Não eleva privilégios por conta própria: qualquer operação administrativa passa por `forge-helper`.

**Quem depende dele:** `forge-daemon`.

**Referência:** [docs/06-sites-vhosts-multi-php.md](../../docs/06-sites-vhosts-multi-php.md), [docs/11-security.md](../../docs/11-security.md) e [ADR-0004](../../docs/adr/0004-dns-nrpt-resolver.md).
