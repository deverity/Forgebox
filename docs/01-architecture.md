# 01 — Arquitetura

## 1. Visão de processos

```
┌──────────────────────────────────────────────────────────────────────┐
│ Windows (sessão do usuário)                                          │
│                                                                      │
│  Forgebox.exe (GUI, Tauri 2)      forge.exe (CLI)      IDE / Agente  │
│        │ JSON-RPC (named pipe)         │                  │ MCP/WS    │
│        └───────────────┬───────────────┴──────────────────┘          │
│                        ▼                                             │
│               forgeboxd.exe (daemon, user-space, instância única)    │
│   ┌────────────┬────────────┬───────────┬──────────┬──────────────┐  │
│   │ supervisor │ fcgi pools │ providers │ sites    │ net (dns/tls)│  │
│   └─────┬──────┴─────┬──────┴─────┬─────┴────┬─────┴──────┬───────┘  │
│         ▼            ▼            ▼          ▼            ▼          │
│   httpd.exe    php-cgi.exe ×N   downloads  vhost files  dns :53      │
│   nginx.exe    (por versão)     (reqwest)  certs        (hickory)    │
│   mysqld.exe   node apps                                             │
│   mailpit.exe  (Procfile)                                            │
│                                                                      │
│  forgebox-helper.exe (elevado, sob demanda ou serviço opcional)      │
│   • NRPT rule .test → 127.0.0.1   • hosts (fallback)   • firewall    │
└──────────────────────────────────────────────────────────────────────┘
```

Regras:
- **Só o daemon toca em processos e arquivos de configuração.** GUI e CLI são clientes finos. Isso garante que fechar a janela não derruba a stack e que CLI e GUI nunca divergem.
- O daemon é **user-space** (sem admin), inicia com o login se o usuário quiser, e é instância única (named mutex `Global\Forgebox.Daemon`).
- Todos os processos filhos vivem dentro de um **Job Object** com `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: se o daemon morrer, nada fica órfão (problema crônico do Laragon com `php-cgi.exe`).
- O helper elevado só existe para três coisas: regra NRPT (DNS wildcard), edição do `hosts` como fallback e regras de firewall. Cada chamada é uma operação nomeada, validada e logada (ver [11-security.md](11-security.md)).

## 2. Stack tecnológica

| Camada | Escolha | Por quê (resumo; detalhes nos ADRs) |
|---|---|---|
| Linguagem do motor | **Rust 2024 edition**, stable | Binário único, sem runtime, controle fino de processos/Win32 via crate `windows` |
| Runtime assíncrono | **tokio** | Supervisão de dezenas de processos, I/O de rede, timers |
| Persistência | **SQLite** via `rusqlite` (bundled) | Estado, sites, portas, histórico de eventos, cache de registry. Um arquivo, zero servidor |
| Tempo | **jiff** (`Timestamp` UTC, RFC 3339 no fio; sem tzdb embutido), re-exportado como `forge_core::Timestamp` ([ADR-0013](adr/0013-jiff-timestamps.md)) | Um único tipo de instante em eventos, SQLite e API; integra com serde e specta |
| Config do usuário | **YAML** (`serde_yaml_ng`, ver [ADR-0012](adr/0012-yaml-crate-serde-yaml-ng.md)) + **TOML** para preferências simples | YAML é o formato do manifesto; preferências em `forgebox.toml` |
| Templates de config | **minijinja** | Sintaxe Jinja2 conhecida, sandbox, sem dependências pesadas |
| HTTP client | **reqwest** (rustls) + resume + mirrors | Downloads grandes com retomada |
| Verificação | `sha2`, `pgp` (rpgp), `minisign-verify`, Authenticode via `WinVerifyTrust` ([ADR-0017](adr/0017-verify-extract-stack.md)) | Tudo em Rust puro; chaves PGP de nginx/Node embutidas |
| Extração | `zip`, `sevenz-rust2`, `tar` + `flate2`/`lzma-rust2` ([ADR-0017](adr/0017-verify-extract-stack.md)) | PHP/Apache/Nginx são zip; alguns upstreams em 7z/tar.gz/tar.xz; sem toolchain C |
| DNS | **hickory-server** | Resolver `*.test` → 127.0.0.1 em `127.0.0.1:53` (ou porta alta + NRPT) |
| TLS/CA | **rcgen** + `windows::Win32::Security::Cryptography` | CA raiz local e certificados folha; instalação no store `CurrentUser\Root` sem admin |
| IPC | **JSON-RPC 2.0** (NDJSON, `request_id` obrigatório, handshake `system.hello`) sobre trait `Transport`: **Named Pipe** (`\\.\pipe\forgebox`) local; WebSocket+TLS/mTLS e túnel SSH para o modo remoto ([13](13-remote-mode.md), [ADR-0009](adr/0009-remote-ready-ipc.md), [ADR-0015](adr/0015-ipc-protocol.md)) | Named pipe tem ACL por usuário; mesma API local e remota |
| GUI | **Tauri 2** + WebView2 (presente em todo Win10/11) | Frontend web permite acabamento Apple-ish; bundle ~10 MB; `window-vibrancy` para Mica/Acrylic. Ver [ADR-0002](adr/0002-gui-tauri.md) |
| Frontend | **React 19 + TypeScript + Vite + Tailwind 4 + Radix Primitives + Motion** | Acessibilidade de graça (Radix), motion com springs, tokens via CSS vars |
| CLI | **clap** v4 + `tabled`/`indicatif` + saída `--json` | UX de CLI moderna; cada comando tem saída humana e JSON |
| Logs | **tracing** + `tracing-appender` (rotação) | Logs estruturados do daemon; logs das tools ficam onde a tool escreve, indexados |
| Updater do app | **tauri-plugin-updater** (releases assinados, `latest.json` no GitHub) | Assinatura minisign; modo passivo |
| Instalador | **NSIS** (via bundler do Tauri) + **zip portátil** | Instalação por usuário sem admin; portátil para pendrive/VM |
| Build/release | `cargo xtask`, GitHub Actions | Reprodutível, artefatos assinados |

## 3. Workspace de crates

```
crates/
  forge-core         # tipos de domínio, IDs, erros, eventos, store SQLite, paths
  forge-manifest     # schema YAML (serde + schemars), parser, validador, resolução de versão
  forge-registry     # índice de manifestos (git/http), busca, cache, discovery de pastas em bin/
  forge-provider     # providers de download (php-windows, github-release, http, apache-lounge, mariadb, mysql, postgresql-edb, nodejs-dist...), verify, extract, layout versionado, junction `current`
  forge-supervisor   # spawn, Job Objects, políticas de restart, health probes, captura de stdout/stderr, graceful stop
  forge-fcgi         # gerenciador de pools FastCGI de PHP (php-cgi.exe) por versão: portas, scaling, recycle, warmup, swap sem downtime
  forge-render       # renderização de httpd.conf/nginx.conf/php.ini/my.ini/postgresql.conf/vhosts; blocos gerenciados; detecção de drift
  forge-sites        # modelo de site, drivers de framework, parked dirs, vhosts, proxies, apps (Procfile)
  forge-net          # alocação de portas, detecção de dono de porta, DNS server, NRPT/hosts (via helper), firewall
  forge-tls          # CA local, certificados folha (SAN + wildcard), trust store, renovação
  forge-ipc          # contratos da API (serde), servidor/cliente JSON-RPC, stream de eventos, auth por token local
  forge-daemon       # bin: forgeboxd.exe — compõe tudo acima
  forge-cli          # bin: forge.exe — cliente da API, com fallback para iniciar o daemon
  forge-helper       # bin: forgebox-helper.exe — operações elevadas, allowlist estrita
  forge-mcp          # servidor MCP (stdio/WS) expondo a API para agentes de IA/IDEs
  forge-metrics      # tail de access logs JSON, histogramas HDR por site/janela, rateio CPU/RAM por vhost, SQLite metrics_* (ver 14)
apps/
  desktop/           # Tauri 2 (src-tauri/ em Rust + src/ em React)
```

Dependências permitidas entre crates (setas = "pode depender de"):

```
forge-core ← forge-manifest ← forge-registry ← forge-provider
forge-core ← forge-supervisor ← forge-fcgi
forge-core ← forge-render ← forge-sites
forge-core ← forge-net, forge-tls
forge-ipc ← (todos os de domínio, só pelos tipos)
forge-daemon → todos
forge-cli, forge-mcp, apps/desktop → forge-ipc apenas (nunca importar crates de domínio diretamente; os tipos base — ids, paths, códigos de erro, Timestamp — vêm do re-export `forge_ipc::base`)
```

Essa regra força que **toda** funcionalidade passe pela API, logo GUI e CLI têm paridade por construção.

## 4. Modelo de domínio (resumo)

```
Tool            manifesto YAML (id, categoria, providers, layout, service spec, config spec, ui)
ToolVersion     uma versão instalada de uma Tool: bin/<tool>/<versão>, estado (installed|downloading|broken), checksum
Instance        um serviço executável de uma ToolVersion com configuração própria: ex. mysql 8.4 "default" na porta 3306, mysql 8.4 "legacy" na porta 3307. Tools sem serviço não têm Instance
PhpPool         pool FastCGI de uma ToolVersion de PHP: N workers, porta(s), métricas
Site            host (ex. app.test), root, driver, web server, versão de PHP (→ PhpPool), SSL, aliases, env
Proxy           Site do tipo proxy: host → upstream http(s)://127.0.0.1:PORT, websocket, headers
App             processo(s) gerenciados ligados a um Site ou soltos (Procfile): web, worker, scheduler
Stack           preset que agrupa Tools/Instances/Sites (ex. "Laravel", "WordPress", "Next.js + API PHP")
Port            reserva lógica de porta: dono (Instance/App/PhpPool), estado, conflito detectado
Certificate     CA raiz + folhas por Site
Event           tudo que acontece: timestamp, origem, tipo, payload. Alimenta UI, logs e auditoria
```

## 5. Fluxos principais

### 5.1 Boot do daemon
1. Adquire mutex; abre `state/forgebox.db`; migra schema.
2. Carrega `forgebox.toml` e índice de manifestos (cache local; refresh em background).
3. Varre `bin/` e reconcilia com o banco (descobre versões instaladas manualmente).
4. Cria Job Object raiz.
5. Sobe DNS server (se habilitado) e verifica NRPT/hosts.
6. Para cada Instance com `autostart: true`: start em ordem topológica (`depends_on`).
7. Para cada versão de PHP referenciada por sites ativos: sobe PhpPool.
8. Renderiza configs (só se houver drift) e sobe web servers.
9. Abre IPC e emite `daemon.ready`.

Implementação (`forge_daemon::boot`, M1-E5-T2, [ADR-0029](adr/0029-daemon-composition.md)): o mutex (`Global\Forgebox.Daemon` ou `Global\Forgebox.Daemon-<chave da raiz>`, a mesma chave do pipe) é criado antes de tudo, e um segundo daemon sai com código 4 em ~50 ms; `ForgeRoot::init` vem logo depois, então os logs. Cada passo que depende do mundo externo tem prazo e, se falhar, vira aviso: catálogo do cache em até 10 s (o carregamento continua em segundo plano; na primeira execução o registry oficial é cadastrado), `discover` em até 20 s, DNS só com `[dns] enabled` (padrão `true`). No passo 6 os servidores web esperam: sobem no 8, depois dos pools, já com os upstreams no primeiro render. Com a API aberta, `state/locks/forgeboxd.lock` descreve o dono (pid, versão, pipe; só informativo, removido na parada). Raiz vazia: ~130–200 ms até o pipe responder.

### 5.2 Criar site com PHP específico
`forge site add C:\www\loja --host loja.test --php 8.3 --server nginx --secure`
1. `forge-sites` detecta driver (Laravel → root `public/`).
2. Reserva host; valida DNS (`loja.test` resolve para 127.0.0.1? senão instrui/aciona helper).
3. `forge-tls` emite cert folha para `loja.test, *.loja.test`.
4. Garante `PhpPool(8.3)` ativo (instala PHP 8.3 se não existir, com confirmação).
5. `forge-render` escreve `etc/nginx/sites/loja.test.conf` apontando `fastcgi_pass php_8_3`.
6. `nginx -s reload` (sem downtime). Evento `site.created`.

### 5.3 Atualização automática de PHP 8.3.36 → 8.3.37
1. Scheduler consulta provider (`releases.json`), encontra 8.3.37 com SHA-256.
2. Política da tool = `auto` para patch → baixa para `tmp/downloads/`, verifica, extrai para `bin/php/8.3.37/`.
3. Copia `php.ini` gerenciado (re-render do template com overrides do usuário) e extensões compatíveis (Xdebug para 8.3 VS17 x64 é o mesmo build).
4. `forge-fcgi` sobe novo pool 8.3.37 em novas portas, faz warmup, troca upstream no web server (reload), drena o pool antigo, encerra.
5. Re-aponta `bin/php/current` se a versão ativa de CLI era 8.3.
6. Mantém 8.3.36 por `retention` (padrão: 1 versão anterior) para rollback. Evento `tool.updated`.

Implementação: `forge_provider::update::UpdateEngine` ([05 §4.1](05-providers-and-updates.md), [ADR-0027](adr/0027-update-engine-active-version-rollback-retention.md)); a política padrão do PHP no registry é `auto-patch`. O passo 3 é `update.migrate` (`copy_dir` de `ext/`) + re-render das ações `render` do `post_install` para a versão nova, antes da troca do passo 4 (`PoolSwitcher`, ligado pelo daemon ao `forge_fcgi::Switcher`); a retenção só roda depois da troca.

### 5.4 Trocar a versão ativa de CLI (Node, PHP, Python...) sem mexer em env-vars
`forge use node 22`
1. Valida que `bin/node/22.x.y` existe (oferece instalar).
2. Troca a junction `bin/node/current` → `bin/node/22.x.y` de forma atômica (cria `current.new`, renomeia).
3. PATH do usuário já contém `C:\Forgebox\bin\node\current` (adicionado uma única vez no onboarding), então novos terminais e o próprio terminal atual já enxergam a nova versão. Nenhuma variável muda, nenhum restart.
4. Shells abertos pelo Forgebox (`forge shell`, terminal integrado) recebem o PATH já montado.

## 6. Observabilidade
- Cada operação do daemon gera `Event` persistido (ring buffer de 50k + arquivo).
- Métricas por processo: CPU, RSS, handles, uptime, restarts (amostragem 2 s, via `GetProcessTimes`/`GetProcessMemoryInfo`/`GetProcessHandleCount`, sem PDH; somadas por Job da instância, [03 §6](03-engine.md)).
- Logs das tools são "tailados" sob demanda pelo daemon e enviados por stream para a GUI/CLI (`forge logs nginx -f`).
- Health de cada Instance: `tcp`, `http`, `command`, `pid`. Resultados alimentam o estado exibido.

## 7. Portabilidade e modo remoto
- Todo acesso a Win32 fica em módulos `platform::windows::*` com traits (`ProcessGroup`, `TrustStore`, `DnsPolicy`, `LinkFs`). Em Linux/macOS as implementações são cgroups/systemd-user ou launchd, `update-ca-certificates`/Keychain, `systemd-resolved`/`/etc/resolver`, symlinks.
- O daemon pode rodar **headless em Linux** e ser controlado pela GUI/CLI do Windows (e vice-versa). A API já nasce "remote-ready" ([ADR-0009](adr/0009-remote-ready-ipc.md)); detalhes em [13-remote-mode.md](13-remote-mode.md).
- Métricas por vhost (req/s, p50/p90/p99, CPU/RAM atribuídos) são produzidas pelo daemon a partir de access logs JSON e do sampler de processos; ver [14-observability-metrics.md](14-observability-metrics.md).
