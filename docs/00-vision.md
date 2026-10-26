# 00 — Visão e análise competitiva

## 1. Tese

Todo ambiente PHP local para Windows hoje obriga o desenvolvedor a escolher entre **controle** (Laragon, XAMPP, WAMP: pastas abertas, mas UI datada, uma versão de PHP por vez para web, updates manuais) e **conveniência** (Herd, ServBay: bonitos e automáticos, mas caixa-preta, pagos para o que importa, pesados ou frágeis no Windows).

**Forgebox** elimina a escolha: um motor em Rust, pequeno e previsível, com um catálogo de ferramentas descrito em YAML, múltiplas versões de **tudo** rodando ao mesmo tempo, atualização automática e uma interface com acabamento de app Apple.

Frase de posicionamento: *"A stack local inteira, sob controle total, com um clique."*

Modelo de distribuição: **instalador pequeno + catálogo completo sob demanda.** O Forgebox não embute PHP, Apache ou MySQL. Ele embute o catálogo. O usuário vê tudo que existe, clica **Instalar**, o motor baixa do provider oficial, verifica, instala lado a lado e ativa. Compatibilidade "completona" sem inflar o download inicial.

## 2. O que estudamos

| Produto | Stack do app | Como roda PHP web | Multi-PHP simultâneo | Downloads de tools | Auto-update tools | UI | Preço |
|---|---|---|---|---|---|---|---|
| **Laragon 8.8** (out/2026) | Delphi, Inno Setup 6.4, ~240 MB | Apache `mod_fcgid` **ou** Nginx → N processos `php-cgi.exe` em porta fixa (10987, fallback 20987) | Não. Uma versão ativa por vez (troca reinicia tudo) | `packages.conf` com URLs fixas + "Quick add" | Parcial (PHP via Auto Update, resto manual) | Win32 clássica, tray menu, sem dark mode nativo | Grátis |
| **Laravel Herd Win 2.x** | **Electron** (~490 MB instalador, ~1,2 GB instalado), HerdHelper (serviço elevado) | Nginx catch-all estilo Valet, variável `$herd_sock` roteia para o php-cgi da versão do site | Sim (`herd isolate`) | Internos, fechados | Sim (`herd php:update`) | Moderna, dark mode, Dumps, Mail, Log viewer | Grátis + Pro (US$ 99/ano) para DBs, Dumps, Mail |
| **ServBay Win 1.34** | Delphi/Inno, ~47 MB (online installer) | Apache/Nginx/Caddy, troca instantânea | Sim | Internos | Sim | Moderna, monitor de recursos | Grátis (5 sites) + Pro US$ 59 |
| **XAMPP 8.2.12** | Bitnami/Inno | Apache `mod_php` | Não | Não | Não (Windows parado desde 11/2023) | Painel Win32 | Grátis |
| **WampServer 3.4** | Inno/Delphi | Apache `mod_php` ou FCGI | Não para web (6 versões instaladas, 1 ativa) | Addons .exe manuais | Não | Tray menu | Grátis |
| **DDEV / Lando** | Go + Docker | Containers | Sim (por projeto) | Imagens Docker | Sim | CLI | Grátis |

Fontes: releases do Laragon no GitHub (8.1 → 8.8), changelog do Herd for Windows (1.1 → 2.0.3), página de comparação da ServBay, payload do instalador do Herd (`app.asar.unpacked/resources`: nginx, nvm, `dnsmasq.conf` com `address=/.test/127.0.0.1`, pool FPM estilo Valet), instalação local do Laragon em `C:\laragon` (`usr/laragon.ini`, `usr/packages.conf`, `usr/sites.conf`, `usr/tpl/*.tpl`, `bin/php/switch-php.ps1`).

### 2.1 O que o Laragon faz bem (e manteremos)
- Layout de pastas transparente: `bin/<tool>/<versão>`, `etc/`, `data/`, `www/`, `usr/`. O usuário sempre sabe onde está cada coisa.
- Portátil, sem Windows Services, tudo em uma pasta.
- Auto virtual hosts: pasta em `www/` vira `nome.test` automaticamente.
- Templates editáveis (`usr/tpl/*.tpl`) e `Procfile` para processos extras.
- Troca de versão de PHP/Node por **junction** `bin/php/php` → versão ativa, para o PATH nunca mudar (`switch-php.ps1`). **Esse é o padrão que o usuário usa hoje e que vamos adotar para todas as tools** (`bin/<tool>/current`).
- "Quick add" de tools a partir de URLs (`packages.conf`) e "Quick app" (`sites.conf`) para criar projetos.

### 2.2 Onde o Laragon falha (e onde ganhamos)
- Uma versão de PHP por vez para web. Trocar reinicia Apache/Nginx e mata processos `php-cgi` de forma pouco confiável (issues #247, #374, #1307).
- Pool de PHP primitivo: N processos em uma porta, sem health, sem recycle, sem scaling.
- Sem DNS wildcard real: depende de editar `hosts` (sem curinga). Subdomínios exigem entradas manuais.
- UI Win32 sem dark mode, sem busca, sem visualização de logs, sem monitor de recursos.
- Catálogo de tools como lista de URLs estáticas; sem checksum, sem metadados, sem descoberta.
- SSL "auto" com um certificado único; sem CA raiz gerenciada com renovação.
- Sem CLI de verdade, sem API para IDEs/agentes.

### 2.3 Onde o Herd falha (e onde ganhamos)
- Electron: pesado, lento para abrir, consome RAM. Relatos de serviços que não sobem e de regressões a cada update no Windows.
- Caixa-preta: pastas internas em `%USERPROFILE%\.config\herd`, configs sobrescritas, pouca customização.
- Recursos essenciais (bancos, mail, dumps) atrás de assinatura.
- Laravel-cêntrico.

### 2.4 Onde o ServBay falha
- Limite de 5 sites no grátis, mail server só no Pro.
- Online installer; proprietário; roadmap voltado a "AI/Ollama" em vez de profundidade na stack.

## 3. Pilares do Forgebox

1. **Motor Rust pequeno e determinístico.** Daemon em user-space, binário único de ~10 MB, zero dependência de Windows Services para a stack, zero Electron. Tudo que acontece é um evento observável e logado.
2. **Multi-versão simultâneo de tudo.** PHP 7.4, 8.3 e 8.5 servindo sites ao mesmo tempo, cada vhost declarando sua versão. Node 18 e 22 em projetos diferentes. MySQL 8 e MariaDB 11 lado a lado. Switching da versão "ativa" de CLI via junction `current`, sem tocar em variáveis de ambiente.
3. **Catálogo aberto via YAML.** Cada tool é um manifesto `forgebox.yaml` (download, verificação, instalação, serviço, configs, UI). Registry oficial versionado em git + registries de terceiros + manifestos embutidos no próprio pacote da tool + descoberta heurística de pastas soltas em `bin/`.
4. **Updates automáticos com rollback.** Políticas por tool (auto / notificar / manual), download verificado por SHA-256 (e assinatura quando o upstream oferece), instalação lado a lado, swap atômico, rollback em um clique. O próprio Forgebox se atualiza com releases assinados.
5. **Rede local de verdade.** DNS wildcard `*.test` via resolvedor próprio + NRPT (sem editar `hosts`), CA raiz local confiável no Windows, HTTPS para todo site, reverse proxy com WebSocket para apps Node/Next/Vite, túneis públicos (cloudflared, ngrok, expose) em um clique.
6. **UI de nível Apple.** Translucência nativa (Mica/Acrylic), tipografia e espaçamento rígidos, motion com física, command palette, tray inteligente, dark/light automático, zero jank. Cada estado de serviço é visível e explicável. Ver [07-ui-ux.md](07-ui-ux.md).
7. **Tudo acessível por CLI e API.** `forge` espelha 100% da GUI. API JSON-RPC local + servidor MCP para IDEs e agentes de IA.
8. **Grátis e aberto no núcleo.** Nenhuma funcionalidade essencial (bancos, mail, SSL, multi-PHP) atrás de paywall.

## 4. Escopo da v1 (Windows x64)

### Catálogo oficial de lançamento (tudo "instale sob demanda")
A lista completa, com provider, formato e observações de compatibilidade está em [12-stacks-compat.md](12-stacks-compat.md). Resumo:

| Categoria | Tools | Observações |
|---|---|---|
| Runtime | PHP 7.4–8.6 (NTS e TS), Composer, Node.js (LTS/Current), Bun, Deno, Python (embeddable), Go, Java (Temurin), .NET SDK, Ruby (RubyInstaller) | PHP via `releases.json` oficial do windows.php.net |
| Web server | Apache 2.4 (Apache Lounge VS17/VS18), Nginx (nginx.org), Caddy, FrankenPHP (nativo Windows ≥ 1.12) | Apache e Nginx podem rodar juntos em portas diferentes |
| Banco | MySQL 8.x/9.x, MariaDB 10.11/11.x, PostgreSQL 15–18 (binários EDB) + PostGIS, MongoDB Community, SQLite (CLI) | Múltiplas instâncias por versão, `data/<tool>/<instância>` |
| Cache / KV / Fila | Redis-compatível nativo: Memurai Developer (API Redis 7); redis-windows 5.0.14 (legado); Memcached; Valkey, DragonflyDB, KeyDB e RabbitMQ: sem build nativo Windows → provider `wsl`/`docker` (fase 2) | Honestidade: não existe Redis/Valkey/Dragonfly nativo e atual para Windows fora do Memurai |
| Mail | Mailpit (SMTP 1025 + UI/API 8025) como catcher local tipo Mailtrap; smtp4dev como alternativa | UI de e-mails embutida no Forgebox consumindo a API do Mailpit |
| Storage / Search | MinIO, RustFS, Meilisearch, Typesense, Elasticsearch/OpenSearch (zip Windows) | Todos com binário Windows oficial |
| Túneis | cloudflared, ngrok, expose | `forge share <site>` |
| Ferramentas | Git (MinGit), HeidiSQL, DBeaver, phpMyAdmin, Adminer, Xdebug, pgAdmin, RedisInsight, Mongo Compass | Xdebug como extensão instalável por versão de PHP |

### Fora do escopo da v1
- macOS/Linux (arquitetura preparada, sem build).
- Containers/WSL como provider (v2).
- Sync de configuração em nuvem / times (v3).

## 5. Métricas de sucesso da v1
- Instalação zero → primeiro `https://meusite.test` respondendo em **< 3 minutos** com download de PHP + Nginx.
- Troca de versão de PHP de um site: **< 2 s**, sem derrubar outros sites.
- Daemon ocioso: **< 40 MB RAM**, GUI aberta **< 150 MB**.
- Cold start da GUI: **< 800 ms** até primeiro frame útil.
- 100% das ações da GUI disponíveis na CLI.
