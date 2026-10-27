# forge-tls

**O que é:** CA local com name constraints (só TLDs de uso local, `localhost` e loopback), certificados folha (host, wildcard e aliases), CRL servida em `127.0.0.1` para o Schannel, confiança no trust store do usuário (por thumbprint, com livro `trust.json`), rotação e renovação diária: o ator `tls` (`TlsManager`). Decisões em [ADR-0023](../../docs/adr/0023-local-ca-name-constraints-trust.md).

**O que não é:** Não usa ACME nem CA pública (previsto só no M5). Não fala Win32: o store do Windows é `forge_core::platform::TrustStore`. Não recarrega servidores web (o daemon reage a `tls.renewed`).

**Quem depende dele:** `forge-daemon`.

**Referência:** [docs/06-sites-vhosts-multi-php.md](../../docs/06-sites-vhosts-multi-php.md) e [docs/11-security.md](../../docs/11-security.md).
