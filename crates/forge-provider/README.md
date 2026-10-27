# forge-provider

**O que é:** Providers de download (http, github-release, php-windows...), verificação (sha256/sha512, PGP com keyrings em `keys/`, minisign, Authenticode), extração segura (zip/7z/tar.gz/tar.xz), layout `bin/<tool>/<versão>` e junction `current`.

**O que não é:** Não sobe processos nem escreve configs. Download sem sha256 verificado não é aceito.

**Quem depende dele:** somente `forge-daemon`.

**Referência:** [docs/05-providers-and-updates.md](../../docs/05-providers-and-updates.md) e [docs/08-release-layout.md](../../docs/08-release-layout.md).
