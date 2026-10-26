# 12 — Catálogo completo e compatibilidade de stacks

Princípio: **compatibilidade "completona", instalação sob demanda.** Tudo abaixo aparece no catálogo. Nada vem no instalador. O usuário clica Instalar → o motor baixa do provider oficial, verifica e ativa. Itens sem build nativo Windows aparecem com badge honesto e caminho alternativo (WSL/Docker) em fase 2.

Legenda de status: ✅ nativo Windows, build oficial · 🟡 nativo via terceiro confiável · 🔵 só via WSL2/Docker (provider v2) · ⚪ GUI/externo lançado pelo Forgebox.

## 1. Runtimes / linguagens

| Tool | Status | Provider | Formato | Multi-versão | `current` junction | Observações |
|---|---|---|---|---|---|---|
| PHP 7.4 → 8.6 | ✅ | `php-windows` (`releases.json` + archives) | zip | sim, pools simultâneos | sim (CLI) | NTS padrão para FastCGI; TS para `mod_php` legado. Exige VC++ redist vc15/vs16/vs17 |
| Composer | ✅ | `github-release` (composer/composer `composer.phar`) + `getcomposer.org` sig | phar + `composer.cmd` gerado | sim | sim | Usa o `php` do `current` ou `--php` |
| Node.js 18/20/22/24 | ✅ | `nodejs-dist` (`index.json`, `SHASUMS256.txt`) | zip | sim | sim | Corepack on; npm-global por versão; `.nvmrc` respeitado |
| Bun | ✅ | `github-release` (oven-sh/bun, `bun-windows-x64.zip`) | zip | sim | sim | |
| Deno | ✅ | `github-release` (denoland/deno) | zip | sim | sim | |
| Python 3.9 → 3.14 | ✅ | `python-embed` (embeddable zip) ou NuGet pkg (com pip) | zip/nupkg | sim | sim | pip habilitado via `get-pip` embutido |
| Go | ✅ | `http` (`go.dev/dl/?mode=json`) | zip | sim | sim | `GOPATH` em `data/go` |
| Java (Temurin JDK 17/21/25) | ✅ | `http` (api.adoptium.net) | zip | sim | sim | `JAVA_HOME` só em shells do Forgebox |
| .NET SDK 8/9/10 | ✅ | `http` (`dotnetcli.blob` releases.json) | zip | sim | sim | |
| Ruby (RubyInstaller) | 🟡 | `github-release` (oneclick/rubyinstaller2, 7z) | 7z | sim | sim | |
| Rust (rustup) | ⚪ | link para rustup-init | — | — | — | Fora do escopo gerenciar toolchains; só atalho |

## 2. Servidores web

| Tool | Status | Provider | Observações |
|---|---|---|---|
| Apache HTTP Server 2.4 | ✅ | `apache-lounge` (VS17 e VS18) | Módulos extras (mod_fcgid, mod_security, mod_http2 já incluso) como `Extension`. Templates usam `mod_proxy_fcgi` + balancer → pools |
| Nginx (stable/mainline) | ✅ | `nginx-org` | PGP verificado. Templates `upstream` por pool |
| Caddy 2 | ✅ | `github-release` (caddyserver/caddy) | HTTPS automático desativado em favor da CA do Forgebox (consistência) |
| FrankenPHP ≥ 1.12 | ✅ | `github-release` (php/frankenphp, binário Windows) | Por site (`runtime: frankenphp`), worker mode, Octane |
| OpenLiteSpeed / LiteSpeed | ❌ | — | Sem Windows. Fora |
| IIS | ⚪ | — | Não gerenciado; detectamos conflito em 80/443 e explicamos |

## 3. Bancos de dados relacionais

| Tool | Status | Provider | Multi-instância | Observações |
|---|---|---|---|---|
| MySQL 8.0 / 8.4 LTS / 9.x Innovation | ✅ | `mysql` (lista curada pelo bot; zip oficial) | sim | `--initialize-insecure` na 1ª vez; `mysql_upgrade` assistido; nunca auto-update |
| MariaDB 10.6 / 10.11 / 11.x | ✅ | `mariadb` (REST oficial, sha256) | sim | Porta sugerida 3307 quando MySQL coexiste |
| PostgreSQL 14 → 18 | ✅ | `postgresql-edb` (binaries zip, curado) | sim | `initdb` com `trust` local; `pg_ctl`; PostGIS como `Extension` (stackbuilder zip) |
| SQLite 3 (CLI + sqldiff) | ✅ | `http` (sqlite.org `download.html` parse) | — | Útil para Laravel/sqlite |
| SQL Server Express / LocalDB | ⚪ | link oficial | — | Instalador MSI próprio; só atalho e detecção de porta 1433 |
| Oracle XE / Firebird | ⚪/🟡 | — / `github-release` | — | Firebird tem zip Windows; baixa prioridade |

## 4. NoSQL, cache, fila, busca

| Tool | Status | Provider | Observações |
|---|---|---|---|
| MongoDB Community 7/8 + mongosh | ✅ | `http` (fastdl.mongodb.org `full.json`) | Zip oficial; Compass como GUI ⚪ |
| **Redis-compatível: Memurai Developer** | 🟡 | `http` (memurai.com, instalador MSI → extraímos via `msiexec /a` para layout portátil) | API Redis 7.x; licença Developer proíbe produção (ok para dev local). **Padrão para "Redis" no Windows** |
| redis-windows 5.0.14 (tporadowski) | 🟡 | `github-release` | Legado, sem manutenção desde 2023; mantido para compat de projetos antigos |
| Redis oficial | 🔵 | `wsl`/`docker` (v2) | Não há build Windows oficial |
| **Valkey** | 🔵 | `wsl`/`docker` (v2) | Documentação oficial: Windows só via WSL |
| **DragonflyDB** | 🔵 | `wsl`/`docker` (v2) | Linux only. UI mostra "Disponível via WSL (fase 2)" com botão desabilitado + link |
| KeyDB | 🔵 | v2 | Idem |
| Memcached 1.6 | 🟡 | `github-release` (builds mingw, ex. nono303/memcached) | Binário Windows de terceiro; hash curado |
| RabbitMQ | 🟡 | `github-release` (rabbitmq-server zip) + Erlang/OTP (`http`) | Exige Erlang; o Forgebox trata como `depends_on` |
| Apache Kafka | 🟡 | `http` (apache mirrors) | Exige Java; scripts `.bat`; baixa prioridade |
| NATS | ✅ | `github-release` | |
| Meilisearch | ✅ | `github-release` | master key gerada |
| Typesense | ✅ | `github-release`/`dl.typesense.org` | |
| Elasticsearch / OpenSearch | ✅ | `http` (artifacts.elastic.co / opensearch.org) | Pesado (~600 MB); JDK embutido |
| MinIO | ✅ | `http` (dl.min.io) | Credenciais geradas |
| RustFS | ✅ | `github-release` | S3 compat em Rust |
| Mailpit | ✅ | `github-release` (axllent/mailpit) | **Catcher de e-mail padrão** (tipo Mailtrap local): SMTP 1025, UI/API 8025; UI nativa no Forgebox |
| smtp4dev | ✅ | `github-release` (rnwood/smtp4dev, win-x64 zip) | Alternativa |
| MailHog | 🟡 | `github-release` | Legado |
| Mailtrap (SaaS) | ⚪ | — | Integração: botão "copiar credenciais Mailtrap para .env" se o usuário informar token; não substitui catcher local |

## 5. Ferramentas de desenvolvedor e GUIs

| Tool | Status | Provider | Observações |
|---|---|---|---|
| Git (MinGit) | ✅ | `github-release` (git-for-windows, `MinGit-*-64-bit.zip`) | Portátil, sem instalador |
| GitHub CLI | ✅ | `github-release` | |
| ngrok | ✅ | `http` (bin.equinox.io) | Token na UI |
| **cloudflared** | ✅ | `github-release` (cloudflare/cloudflared) | Quick tunnel sem conta; túneis nomeados com conta; **provider padrão de `forge share`** |
| expose (Beyond Code) | 🟡 | `github-release` (phar) | Precisa PHP |
| Xdebug | ✅ | `http` (xdebug.org/files) | `Extension` de php por série/variante |
| PCOV, Redis ext, Mongo ext, Swoole/OpenSwoole (não Windows), Imagick (DLL + ImageMagick) | ✅/❌ | `http` (pecl Windows builds em downloads.php.net/~windows/pecl) | `Extension`; o catálogo marca quais existem para cada série |
| phpMyAdmin 5/6 | ✅ | `http` (files.phpmyadmin.net) | Web app servida em `forgebox.test/phpmyadmin` com PHP escolhido |
| Adminer | ✅ | `github-release` | Single-file |
| HeidiSQL | ✅ | `http` (heidisql.com portable zip) | GUI ⚪ lançada pelo Forgebox |
| DBeaver CE | ✅ | `github-release` (zip win32) | GUI |
| TablePlus | ⚪ | detecção se instalado | GUI |
| pgAdmin 4 | 🟡 | `http` (zip portable não oficial) → usamos instalado se existir | GUI |
| RedisInsight | ✅ | `github-release`/download.redis.io | GUI |
| MongoDB Compass | ⚪ | link | GUI |
| Notepad++ | ✅ | `github-release` (portable zip) | Editor leve |
| VS Code / PhpStorm / Cursor | ⚪ | detecção | Editor padrão para "abrir no editor" |
| Windows Terminal | ⚪ | detecção | Terminal externo preferido |
| 7-Zip (lib) | — | embutido (`sevenz-rust`) | Não precisamos do binário |
| OpenSSL CLI | ✅ | vem com Apache Lounge / `http` (slproweb) | Expor no PATH via shim |
| wkhtmltopdf, Chromium headless (Puppeteer/Dompdf) | 🟡 | `github-release` | Para projetos PHP de PDF |
| ImageMagick | ✅ | `http` (imagemagick.org portable zip) | Para Imagick |
| FFmpeg | ✅ | `github-release` (BtbN builds) | |
| Ollama | ✅ | `github-release` | Opcional; categoria `ai`, baixa prioridade |

## 6. Stacks (presets)

| Stack | Composição padrão | Site/processos |
|---|---|---|
| Laravel | PHP 8.4 + Composer + Nginx + MySQL 8.4 + Memurai + Mailpit + Node 22 | `composer create-project`; `.env` preenchido; processes `vite`, `queue`, `schedule` |
| Laravel + Octane (FrankenPHP) | + FrankenPHP | `runtime: frankenphp`, worker mode |
| WordPress | PHP 8.3 + Apache (ou Nginx) + MariaDB 11 + Mailpit | Download `latest.zip`, `wp-config.php` gerado, WP-CLI incluso |
| Symfony | PHP 8.4 + Composer + Nginx + PostgreSQL 17 + Mailpit | `symfony` CLI opcional |
| CakePHP | PHP 8.3 + Composer + Apache + MySQL 8.4 | Driver `cakephp` (root `webroot/`) — relevante para os projetos do usuário em `C:\laragon\www` |
| Drupal / Magento / Craft / Statamic | drivers dedicados | |
| PHP genérico | PHP + Nginx | `index.php` |
| Next.js | Node 22 + proxy site | `processes.web: npm run dev`, `proxy: true`, HMR via WS |
| Nuxt / SvelteKit / Astro / Vite SPA | Node 22 + proxy ou static | |
| Next.js + API PHP | Node 22 + PHP 8.4 + Nginx | `app.loja.test` → Next; `api.loja.test` → PHP; mesma CA, cookies cross-subdomain ok |
| Node API (Express/Nest/Fastify) + PostgreSQL | Node 22 + PostgreSQL | |
| Bun / Deno app | Bun/Deno + proxy | |
| Python (Django/FastAPI) | Python 3.13 + proxy + PostgreSQL | `processes.web: uvicorn ...` |
| Go / .NET / Java app | runtime + proxy | genérico |
| Static site | Nginx/Caddy | `file_server` |

## 7. Compatibilidade de servidores web × runtimes

| | Apache | Nginx | Caddy | FrankenPHP |
|---|---|---|---|---|
| PHP via pool (multi-versão) | `mod_proxy_fcgi` + balancer ✅ | `fastcgi_pass upstream` ✅ | `php_fastcgi` ✅ | não usa pool (embutido) |
| PHP `mod_php` (TS) | opcional legado | — | — | — |
| `mod_fcgid` por vhost | opcional legado | — | — | — |
| Reverse proxy + WebSocket | `mod_proxy_wstunnel`/`upgrade=websocket` ✅ | ✅ | ✅ | ✅ (Caddy) |
| HTTP/2 | `mod_http2` ✅ | ✅ | ✅ | ✅ |
| HTTP/3 | — | ✅ (quic) | ✅ | ✅ |
| `.htaccess` | ✅ | ❌ (driver traduz regras comuns p/ WordPress/Laravel) | ❌ | ❌ |
| Dois rodando juntos | portas alternativas ou frontal-proxy ✅ | ✅ | ✅ | ✅ |

## 8. Fase 2: providers `wsl` e `docker`
- `wsl`: o Forgebox cria/gerencia uma distro própria (`Forgebox-WSL`, Ubuntu minimal importada via `wsl --import`), instala a tool com `apt`/binário Linux, roda como process supervisionado via `wsl.exe -d Forgebox-WSL --exec ...`, expõe porta via localhost forwarding do WSL2 (automático) ou `netsh portproxy`. Dados em `data/wsl/<tool>` montados em `/mnt/...`. Permite **Valkey, DragonflyDB, KeyDB, Redis oficial, RabbitMQ sem Erlang Windows** com a mesma UI de instância/porta/logs.
- `docker`: se Docker Desktop/Podman está presente, `image:` + `ports:` + `volumes:` no manifesto; o supervisor fala com a API do Docker. Mesmo card na UI, badge "container".
- Os dois são **opt-in** e marcados claramente; o núcleo nunca depende deles.
