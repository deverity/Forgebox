# 10 — Roadmap e tarefas para agentes

Este documento quebra o produto em milestones e épicos executáveis por agentes em paralelo. Cada tarefa tem: crate/pasta, dependências, entregável, critérios de aceite. Antes de executar qualquer tarefa, o agente lê `CLAUDE.md`, `docs/README.md` e os docs referenciados na tarefa.

Convenção de IDs: `M<milestone>-E<épico>-T<tarefa>`. Estados: `todo | doing | review | done`. O estado vive em `docs/STATUS.md` (criado pelo primeiro agente que iniciar M0).

---

## M0 — Fundação do repositório (1 agente, sequencial)
**Objetivo:** workspace compila, CI roda, contratos existem.

- **M0-E1-T1 Workspace Cargo.** Criar `Cargo.toml` (workspace, `resolver = "3"`, `edition = "2024"`), `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.editorconfig`, `.gitignore` (target, node_modules, `apps/desktop/dist`, `*.local.toml`). Crates vazios listados em [02](02-project-structure.md) com `lib.rs` e README de 10 linhas. **Aceite:** `cargo build --workspace` ok; `cargo clippy -- -D warnings` ok; `cargo deny check` ok.
- **M0-E1-T2 xtask.** `cargo xtask schema` (gera JSON Schema dos manifestos via `schemars`), `cargo xtask gen-ts` (contratos IPC → TS via `specta`), `cargo xtask lint-registry`. **Aceite:** comandos existem e rodam (podem gerar vazio).
- **M0-E1-T3 CI.** `.github/workflows/ci.yml` (windows-latest: fmt, clippy, test, lint-registry, pnpm build do desktop). **Aceite:** verde no PR; localmente: `scripts/ci.ps1` passa; YAML validado.
- **M0-E1-T4 forge-core base.** `paths.rs` (ForgeRoot com todos os subdiretórios de [08](08-release-layout.md) §2), `ids.rs` (ULID newtypes), `error.rs`, `event.rs` (EventBus com `tokio::sync::broadcast`), `store/` (rusqlite + migrations `0001_init.sql` com tabelas tools, tool_versions, instances, sites, ports, events, update_history, registries), `config.rs` (`forgebox.toml` com defaults). Trait `platform::{ProcessGroup, LinkFs, TrustStore, DnsPolicy, CaseSensitiveFs}` + impl Windows de `LinkFs` (junction via `CreateFile`+`FSCTL_SET_REPARSE_POINT` ou crate `junction`) e de `CaseSensitiveFs` (`NtSetInformationFile`/`FileCaseSensitiveInformation`, [15](15-linux-parity-case-sensitivity.md) §3). **Aceite:** testes de paths e migrations; junction cria/troca/remove em tmp; diretório marcado CS distingue `A.txt` de `a.txt`; `ForgeRoot::init()` cria `www/ etc/ data/ logs/ tmp/` já com a flag.
- **M0-E1-T5 Esqueleto de concorrência.** `forge-core::actor` (trait `Actor`, mailbox mpsc, `ask()` com timeout obrigatório, supervisor de atores com restart e `catch_unwind`), `forge-core::jobs` (`JobId`, progresso, cancelamento, eventos `job.*`), thread de escrita SQLite com fila. Configuração do runtime tokio e `tokio-console` em dev. **Aceite:** teste de ator travado não afeta outro; teste de pânico reinicia ator; lint CI (`clippy::await_holding_lock` como erro).

## M1 — Motor headless + CLI (paralelizável em 5 agentes após M0)
**Objetivo:** `forge tool install php@8.4 nginx`, `forge site add`, `https://x.test` funcionando, sem GUI.

### E1 Manifesto e registry (`forge-manifest`, `forge-registry`, `registry/`)
- **M1-E1-T1 Schema v1** conforme [04](04-tool-manifest-spec.md): structs serde para Tool/Extension/Stack/Project/Registry; `schemars`; validação semântica; `lint.rs` com as 7 regras. **Aceite:** fixtures válidos/inválidos em `tests/`; `xtask schema` gera `registry/schema/*.json`.
- **M1-E1-T2 Manifestos iniciais**: `php`, `apache`, `nginx`, `caddy`, `composer`, `nodejs`, `mysql`, `mariadb`, `postgresql`, `memurai`, `mailpit`, `xdebug` (Extension), stacks `laravel`, `wordpress`, `cakephp`, `nextjs`. Valores de URL/hash reais (verificar manualmente os hashes de pelo menos uma versão de cada). Templates `my.ini` com `lower_case_table_names=0` quando `data/` for CS e **validação empírica** de que MySQL 8.4 e MariaDB 11 iniciam assim em NTFS-CS (documentar resultado em [15](15-linux-parity-case-sensitivity.md) §5; fallback `=2`). **Aceite:** `xtask lint-registry` ok; `index.yaml` gerado; resultado da validação registrado.
- **M1-E1-T3 Registry loader**: fontes git/http/dir, cache em `state/registry/`, verificação minisign (chave de dev em `registry/keys/dev.pub`), busca. **Aceite:** teste com registry local em `tests/fixtures/registry`.
- **M1-E1-T4 Discover**: heurísticas para pastas em `bin/` (incluindo nomes do Laragon). **Aceite:** reconhece `php-8.3-Win32-vs16-x64` copiada de `C:\laragon\bin\php` (fixture com `php.exe` fake que imprime versão).

### E2 Providers e instalação (`forge-provider`)
- **M1-E2-T1 Download engine**: fila, resume, paralelismo, progresso, mirrors, proxy do sistema. **Aceite:** teste com servidor HTTP local (axum) simulando `Range`, falha e retomada.
- **M1-E2-T2 Verify + extract**: sha256/512, PGP (crate `sequoia-openpgp` ou `pgp`), minisign, Authenticode (via `WinVerifyTrust`); extração zip/7z/tar com proteção zip-slip; `strip_root: auto`. **Aceite:** fixtures maliciosas rejeitadas.
- **M1-E2-T3 Providers**: `php-windows` (parse real de `releases.json`, formato documentado em [05](05-providers-and-updates.md) §1), `github-release`, `http`, `nodejs-dist`, `mariadb`, `apache-lounge`, `nginx-org`, `mysql` (curado), `postgresql-edb` (curado). **Aceite:** testes com respostas gravadas (fixtures JSON/HTML) e um teste `#[ignore]` online por provider.
- **M1-E2-T4 Layout + current**: `bin/<tool>/<nome canônico>`, junction `current`, shims `.cmd`, PATH do usuário (HKCU + broadcast) idempotente. **Aceite:** instalar duas versões fake, `use` alterna junction em < 50 ms, PATH contém entrada única.
- **M1-E2-T5 Update engine**: políticas, retenção, histórico, rollback, migrate de `ext/`. **Aceite:** cenário 8.3.36 → 8.3.37 → rollback com pool fake.

### E3 Supervisor e pools (`forge-supervisor`, `forge-fcgi`)
- **M1-E3-T1 Job Objects + spawn + captura de stdio + stop em cascata** conforme [03](03-engine.md) §1, implementado como ator por Instance ([03 §7](03-engine.md)). **Aceite:** matar o daemon mata filhos (teste com processo fake que ignora CTRL-C); CTRL_BREAK funciona em `php-cgi` real; durante um stop travado de 15 s, `forge status` responde em < 50 ms; assinante de logs lento não afeta o processo nem outros clientes.
- **M1-E3-T2 Health probes + máquina de estados + políticas de restart + métricas.** **Aceite:** testes de transição; métricas CPU/RSS batem com Task Manager ±10%.
- **M1-E3-T3 Pré-checagens**: validação de config, portas (`GetExtendedTcpTable` → pid/exe), vcredist (registro `VC\Runtimes\x64`). **Aceite:** porta ocupada retorna `Failed(PortInUse{pid, exe})`.
- **M1-E3-T4 PhpPool**: workers por porta, scaling, recycle, warmup via FastCGI (`FCGI_GET_VALUES` + GET health), geração do bloco upstream, blue/green. **Aceite:** teste e2e com PHP real baixado: 2 pools (8.3 e 8.4) simultâneos; `ab`/`oha` com 200 req sem erro durante swap de versão.

### E4 Render, sites, rede, TLS (`forge-render`, `forge-sites`, `forge-net`, `forge-tls`)
- **M1-E4-T1 Templates** Nginx/Apache/Caddy (base + snippets + upstream + site PHP + site proxy + site static) com **access log JSON `forgebox`** e `FORGEBOX_SITE` nos params ([14](14-observability-metrics.md) §2), php.ini overlay, my.ini, postgresql.conf, blocos gerenciados + drift. **Aceite:** `nginx -t` e `httpd -t` passam com os renders (teste e2e com binários reais).
- **M1-E4-T2 Drivers**: laravel, symfony, wordpress, cakephp, drupal, magento, generic-php, static, spa; parked watcher (`notify`); `forgebox.yaml` Project loader. **Aceite:** fixtures de projetos → VhostModel esperado (snapshot tests).
- **M1-E4-T3 DNS server** (hickory) + NRPT via helper + hosts fallback + diagnóstico. **Aceite:** `Resolve-DnsName qualquer.test` → 127.0.0.1 após `forge dns enable` (teste manual documentado + teste automático do servidor).
- **M1-E4-T4 TLS**: CA com name constraints, folhas, trust `CurrentUser\Root`, renovação. **Aceite:** `curl https://loja.test` sem `-k` passa; `openssl x509` mostra constraints.
- **M1-E4-T6 Paridade Linux**: conversão de diretório existente para CS (mover para dir novo marcado, verificação, reversão), badge/estado por site, `forge fs case-sensitive *`, `forge site case-sensitive`, **Case Mismatch Finder** (`forge site lint-case [--fix]`) para PHP/JS/HTML/CSS/Composer/PSR-4, Git `core.ignorecase=false` em `forge new`, LF/UTF-8/`/` em tudo gerado. **Aceite:** fixture com `Views/header.php` vs `views/Header.php` detectada e corrigida; conversão de `C:\laragon\www` (cópia em tmp) preserva tudo e distingue caixa depois.
- **M1-E4-T5 Proxies + processes** (Procfile 2.0, `$PORT`, `runtime:` PATH, autostart on-visit). **Aceite:** Next.js `create-next-app` em `https://app.test` com HMR funcionando.

### E5 Daemon, IPC, CLI, helper (`forge-ipc`, `forge-daemon`, `forge-cli`, `forge-helper`)
- **M1-E5-T1 Contratos IPC** (todos os grupos de [09](09-cli.md)) + trait `Transport` com impl named pipe + token + eventos com `seq`/`resume_from` + regras remote-ready de [ADR-0009](adr/0009-remote-ready-ipc.md) (ULIDs, `request_id`, `local_only`, perfis, `fs.*` restrito). **Aceite:** `gen-ts` gera tipos; cliente Rust round-trip; teste de reassinatura desde `seq`.
- **M1-E5-T2 Daemon**: boot conforme [01](01-architecture.md) §5.1, scheduler, mutex, shutdown gracioso, logs tracing. **Aceite:** `forgeboxd` sobe/desce limpo 100× em loop sem vazamento.
- **M1-E5-T3 CLI completa** com saída humana e `--json`, completions, `doctor`. **Aceite:** cada comando de [09](09-cli.md) tem teste de integração (contra daemon em root temporário).
- **M1-E5-T4 Helper elevado**: allowlist, validação, log, invocação `runas`. **Aceite:** tentativa de op fora da allowlist falha; teste de validação de parâmetros.
- **M1-E5-T5 Import do Laragon** (`forge import laragon`). **Aceite:** importa `C:\laragon` do usuário (vhosts `auto.*.conf`, `bin\php\*`, `data\mysql-8`) com relatório.

**Gate M1:** script `scripts/smoke.ps1` faz do zero: instala php@8.4 + nginx + mysql@8.4 + mailpit, cria site Laravel, abre `https://loja.test`, roda `php artisan migrate`, envia e-mail capturado no Mailpit. Tudo via CLI. (Uso, modos e resultado: [16](16-smoke-m1.md); correções e decisões: [ADR-0032](adr/0032-m1-gate-integration.md).)

## M2 — GUI (3 agentes em paralelo após M1-E5-T1)
- **M2-E1 Shell do app**: Tauri 2 setup, titlebar custom, vibrancy + fallback, sidebar, roteamento, tema light/dark, tokens de [07](07-ui-ux.md), primitives, command palette, tray, autostart, updater. **Aceite:** checklist §11 de [07]; cold start < 800 ms.
- **M2-E2 Telas núcleo**: Onboarding, Dashboard, Serviços (+detalhe), Sites (+detalhe com troca de PHP ao vivo), PHP, Catálogo (instalar com progresso). **Aceite:** cada tela com estados vazio/carregando/erro; paridade com CLI (mostra comando).
- **M2-E3 Telas complementares**: Bancos, Mail (Mailpit nativo), Logs, Terminal (xterm + ConPTY), Configurações, Diagnóstico. **Aceite:** idem.
- **M2-E5 Métricas v1** (`forge-metrics`): tail dos access logs JSON, histogramas HDR por site/janela, rateio CPU/RAM por vhost (modo shared ≈ / dedicated), tabelas `metrics_1m/1h`, stream `metrics.site`, aba **Desempenho** do site, seção Projetos no Dashboard, `forge stats`, `forge top`. **Aceite:** 1.000 req/s sintéticas com < 2 % CPU do daemon; p99 bate com `oha` ±5 %.
- **M2-E4 Design QA**: revisão de UI em 100/125/150 % DPI, light/dark/vibrancy-off, reduced-motion, teclado. Entregável: relatório + correções.

## M3 — Catálogo completo e stacks (2 agentes)
- **M3-E1** Todos os manifestos de [12](12-stacks-compat.md) com status ✅/🟡 (inclui PostGIS, phpMyAdmin/Adminer como web apps, GUIs, túneis, PECL extensions). Bot do registry (workflow `bump.yml`).
- **M3-E2** Stacks completas + `forge new` + drivers restantes + páginas internas `forgebox.test`.
- **M3-E3** `forge share` (cloudflared/ngrok/expose), LAN expose + QR, backups agendados, bundles offline.

## M4 — Release 1.0
- **M4-E1** Instalador NSIS (por usuário + pasta única), portátil, desinstalador com as 3 opções, assinatura minisign (+ Authenticode se houver cert), `latest.json`, `release.yml`.
- **M4-E2** Documentação do usuário (site estático em `docs-site/`), vídeos curtos, página de comparação.
- **M4-E3** Beta fechado: telemetria opt-in, relatório de bugs com `forge doctor --export`.

## M5 — Pós-1.0
- Providers `wsl`/`docker` (Valkey, DragonflyDB, RabbitMQ), ACME para domínios públicos, Dumps interceptor (Ray-like) para PHP, MCP server rico, Windows ARM64, macOS.
- **Métricas v2** ([14](14-observability-metrics.md)): instrumentação profunda opt-in (memória pico por request), slow queries correlacionadas, alertas locais, OpenMetrics.

## M6 — Modo remoto ([13](13-remote-mode.md))
- **M6-E1** `platform::linux` (cgroups, symlinks, systemd-user, `update-ca-certificates`, `systemd-resolved`) + providers Linux (static-php-cli, apt) + build headless `forge-linux-x64.tar.gz`. **Aceite:** smoke M1 roda em Ubuntu 24.04 via CLI.
- **M6-E2** Transporte SSH (`russh`) até unix socket remoto; perfis em `remotes.toml` + `remotes/<nome>/` na pasta de dados do cliente (ver [13](13-remote-mode.md) §3.2); seletor de contexto na GUI; `local_only` tratado; zona `*.<remote>.test` no resolver local. **Aceite:** GUI no Windows cria site e troca PHP em VM Linux; logs e métricas em tempo real.
- **M6-E3** WSS + mTLS com pareamento SPAKE2, perfis admin/operator/viewer, revogação, painel web mínimo em `/_forgebox/`. **Aceite:** testes de autorização por método; pareamento expira; fingerprint pinado.

---

## Regras para agentes executando tarefas
1. Uma tarefa = um PR (ou commit atômico) com título `M1-E3-T4: PhpPool blue/green`.
2. Nunca inventar formato de manifesto, nome de pasta, nome de comando ou evento: consultar os docs. Se faltar, propor no PR alterando o doc correspondente **no mesmo PR**.
3. Teste é parte do entregável. Sem teste, tarefa fica em `review`.
4. Código Win32 só em `forge-core/src/platform/windows/`.
5. Mensagens de usuário (CLI/GUI) em pt-BR e en-US via i18n; logs em inglês.
6. Ao concluir, atualizar `docs/STATUS.md` e, se a implementação divergir do doc, atualizar o doc.
