# 02 — Estrutura do repositório

Monorepo Cargo workspace + app Tauri + registry de manifestos. Tudo que um agente precisa saber para colocar um arquivo no lugar certo.

```
Forgebox/
├── Cargo.toml                    # workspace: members = ["crates/*", "xtask"]; apps/desktop/src-tauri entra no workspace em M2-E1
├── Cargo.lock
├── rust-toolchain.toml           # channel = "stable", components = [rustfmt, clippy]
├── rustfmt.toml / clippy.toml / deny.toml (cargo-deny: licenças e advisories)
├── .editorconfig
├── .gitignore
├── CLAUDE.md                     # convenções para agentes (aponta para docs/)
├── README.md                     # pitch curto + links
├── LICENSE                       # MIT (núcleo)
│
├── docs/                         # ESTA pasta. Fonte da verdade. Ver docs/README.md
│   ├── adr/                      # Architecture Decision Records (0001-..., 0002-...)
│   └── design/                   # mockups, tokens exportados, specs de tela (png/svg/md)
│
├── crates/
│   ├── forge-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── ids.rs            # ToolId (slug [a-z0-9-]) + newtypes ULID: VersionId, InstanceId, SiteId, JobId, EventId,
│   │       │                     #   PortReservationId, RequestId (re-exportado por forge-ipc; feature `specta` p/ gen-ts)
│   │       ├── paths.rs          # ForgeRoot + todos os caminhos (bin/, etc/, data/...), discover(), init() com flag CS
│   │       ├── error.rs          # ForgeError (thiserror), code() estável para API, exit_code() da CLI (docs/09)
│   │       ├── event.rs          # Event{id, seq, ts, kind, payload}, EventKind (nomes de docs/03 §8), EventBus (tokio broadcast)
│   │       ├── actor/            # trait Actor, ActorRef (tell sem espera; ask com timeout obrigatório), ActorSupervisor
│   │       │                     #   (catch_unwind, RestartPolicy, actor.restarted|failed, stop gracioso); tabela de ordem de travas
│   │       ├── jobs/             # JobRegistry: JobId imediato, JobProgress, cancelamento cooperativo, eventos job.* (payload.rs, v1)
│   │       ├── sync.rs           # OrderedMutex/LockLevel: travas curtas com ordem verificada por debug_assert
│   │       ├── runtime.rs        # build_runtime (max(4, núcleos) workers, 2 × núcleos blocking, threads nomeadas); feature `console`
│   │       ├── store/            # SQLite: migrations/*.sql (include_str), runner com backup, repos por agregado (events.rs...),
│   │       │                     #   writer.rs (thread de escrita, lotes com SAVEPOINT), reader.rs (pool query_only p/ spawn_blocking)
│   │       ├── model/            # Tool, ToolVersion, Instance, Site, Proxy, App, Port, Certificate
│   │       ├── config.rs         # forgebox.toml (preferências, faixas de portas nomeadas)
│   │       └── platform/         # traits (LinkFs, CaseSensitiveFs, CodeSign, ProcessGroup, TrustStore, DnsPolicy) + current();
│   │                             #   impl windows/: link_fs.rs (crate junction), case_sensitive_fs.rs (NtSetInformationFile),
│   │                             #   authenticode.rs (WinVerifyTrust + catálogos, publisher via CryptQueryObject)
│   ├── forge-manifest/
│   │   └── src/
│   │       ├── schema/           # structs serde do forgebox.yaml (v1), schemars → JSON Schema: common, tool,
│   │       │                     #   extension, stack, project, registry; enum Manifest (despacho por `kind`)
│   │       ├── parse.rs          # load_str (erros com linha/coluna) + validação semântica (ids, versões, portas, templates)
│   │       ├── version.rs        # ranges semver, canais, ordenação de builds (vs16/vs17, ts/nts), canonical_name
│   │       ├── lint.rs           # as 7 regras de lint sobre uma pasta de registry (RegistryDir, LintContext)
│   │       ├── issue.rs          # IssueCode (código estável = chave i18n, regra 1..7) e ValidationIssue
│   │       ├── template.rs       # leitura das referências {{ ... }} dos manifestos
│   │       └── yaml.rs           # único módulo que usa serde_yaml_ng (ADR-0012)
│   ├── forge-registry/
│   │   └── src/
│   │       ├── index.rs          # index.yaml do registry, cache em state/registry/
│   │       ├── source.rs         # fontes: git (https), http (index.json), dir local
│   │       ├── search.rs         # busca por nome/categoria/tag, ranking
│   │       └── discover.rs       # heurísticas para pastas soltas em bin/ (php.exe, httpd.exe, mysqld.exe...)
│   ├── forge-provider/
│   │   ├── keys/                 # keyrings PGP embutidos (nginx.asc, nodejs.asc), fingerprints comentados e fixados por teste
│   │   └── src/
│   │       ├── provider/         # trait Provider + impls: http, github_release, php_windows, apache_lounge,
│   │       │                     #   nginx_org, mysql, mariadb, postgresql_edb, nodejs_dist, python, composer, mailpit...
│   │       ├── download.rs       # fila (ator), paralelismo, resume (Range), progresso, mirrors; submódulos download/{transfer,limiter,progress,messages}.rs
│   │       ├── verify.rs         # tamanho, sha256/sha512, minisign, Authenticode (platform::CodeSign); verify/pgp.rs (rpgp, keyrings)
│   │       ├── extract.rs        # zip/7z/tar.gz/tar.xz/arquivo único; strip_root; staging em tmp/stage e move atômico;
│   │       │                     #   extract/{entry (nomes seguros), formats, install (strip, rename, cópia entre volumes)}.rs
│   │       ├── messages.rs       # textos de progresso pt-BR de verify/extract (até existir i18n)
│   │       ├── layout.rs         # bin/<tool>/<version>, junction current, PATH shims
│   │       └── update.rs         # checagem periódica, políticas, retenção, rollback
│   ├── forge-supervisor/
│   │   └── src/
│   │       ├── job.rs            # Job Object raiz e por instância
│   │       ├── process.rs        # spawn com env/cwd/stdio capturado, console oculto
│   │       ├── stop.rs           # estratégias: ctrl-c (GenerateConsoleCtrlEvent), comando, WM_CLOSE, kill
│   │       ├── health.rs         # probes tcp/http/command/pid com backoff
│   │       ├── policy.rs         # restart: never|on-failure|always; limites; cooldown
│   │       ├── metrics.rs        # cpu/rss/handles por pid
│   │       └── service.rs        # máquina de estados da Instance
│   ├── forge-fcgi/
│   │   └── src/
│   │       ├── pool.rs           # PhpPool: workers php-cgi.exe, portas, scaling
│   │       ├── worker.rs         # ciclo de vida de um worker, max_requests, env PHPRC/PHP_INI_SCAN_DIR
│   │       ├── probe.rs          # FCGI_GET_VALUES / request de health
│   │       └── swap.rs           # blue/green entre versões, drenagem
│   ├── forge-render/
│   │   ├── templates/            # *.j2 embutidos (include_dir): apache/, nginx/, caddy/, php/, mysql/, mariadb/, postgresql/, redis/, mailpit/
│   │   └── src/
│   │       ├── engine.rs         # minijinja env, filtros (win_path, posix_path, quote...)
│   │       ├── managed.rs        # blocos "# forgebox:begin/end", preservação de edições do usuário
│   │       ├── drift.rs          # hash do render vs arquivo em disco
│   │       └── targets/          # um módulo por tool conhecida: o que renderiza e para onde
│   ├── forge-sites/
│   │   └── src/
│   │       ├── site.rs           # criação/edição, validação de host, aliases
│   │       ├── drivers/          # detecção de framework: laravel, symfony, wordpress, cakephp, drupal, magento, static, spa, generic
│   │       ├── parked.rs         # watcher (notify) em diretórios "parked" → sites implícitos
│   │       ├── vhost.rs          # gera modelo de vhost (independente de servidor) → forge-render
│   │       ├── proxy.rs          # sites proxy (Node/Next/Vite...), websocket, headers
│   │       ├── apps.rs           # Procfile / processes: web, worker, scheduler, com $PORT
│   │       └── project_file.rs   # leitura de forgebox.yaml do projeto (kind: Project)
│   ├── forge-net/
│   │   └── src/
│   │       ├── ports.rs          # alocador por faixas, reservas, GetExtendedTcpTable (quem usa a porta)
│   │       ├── dns/              # servidor hickory: zonas *.test (configurável), upstream passthrough
│   │       ├── policy.rs         # NRPT via helper; hosts fallback; verificação (resolve de teste)
│   │       └── firewall.rs       # regras via helper (netsh/API) só se o usuário quiser acesso LAN
│   ├── forge-tls/
│   │   └── src/
│   │       ├── ca.rs             # geração/rotação da CA (ECDSA P-256, 10 anos), armazenamento em certs/ca/
│   │       ├── leaf.rs           # cert por site: SAN host + *.host, 825 dias, renovação automática
│   │       └── trust.rs          # CurrentUser\Root (sem admin) + opcional LocalMachine via helper; Firefox/NSS; NODE_EXTRA_CA_CERTS
│   ├── forge-ipc/
│   │   └── src/
│   │       ├── api/              # contratos: métodos JSON-RPC agrupados (tools, sites, services, php, net, tls, system)
│   │       ├── events.rs         # assinatura de eventos (subscribe/unsubscribe), filtros
│   │       ├── server/           # named pipe (windows) + ws (tokio-tungstenite), auth token em state/ipc.token
│   │       └── client/           # cliente usado por CLI, GUI (via Tauri), MCP
│   ├── forge-daemon/             # bin forgeboxd.exe: main.rs, boot.rs, scheduler.rs, tray-less
│   ├── forge-cli/                # bin forge.exe: commands/*.rs (um por grupo), output.rs (table|json|plain)
│   ├── forge-helper/             # bin forgebox-helper.exe: allowlist de ops, manifest UAC, logging
│   └── forge-mcp/                # bin forge-mcp.exe: expõe tools MCP (list_sites, create_site, logs, ...)
│
├── apps/
│   └── desktop/
│       ├── package.json          # pnpm; react, typescript, vite, tailwind, radix, motion, zustand, tanstack-query
│       ├── index.html
│       ├── src/
│       │   ├── main.tsx
│       │   ├── app/              # roteamento (tanstack-router), shell (sidebar, titlebar, command palette)
│       │   ├── design/           # tokens.css, themes (light/dark/vibrancy), primitives/ (Button, Card, Switch...)
│       │   ├── features/         # dashboard/, services/, sites/, php/, databases/, mail/, logs/, catalog/, terminal/, settings/
│       │   ├── ipc/              # cliente tipado gerado dos contratos (ver xtask gen-ts)
│       │   ├── state/            # stores (zustand), queries (tanstack-query), event bridge
│       │   └── i18n/             # pt-BR, en (fluent ou i18next)
│       ├── src-tauri/
│       │   ├── Cargo.toml
│       │   ├── tauri.conf.json   # janela, vibrancy, updater, bundle NSIS, identifier dev.forgebox.app
│       │   ├── capabilities/     # permissões Tauri 2 por janela
│       │   ├── icons/
│       │   └── src/
│       │       ├── main.rs / lib.rs
│       │       ├── commands.rs   # comandos Tauri → forge-ipc client (nunca lógica de domínio aqui)
│       │       ├── tray.rs       # ícone/menu de bandeja, estados, ações rápidas
│       │       ├── window.rs     # vibrancy/mica, titlebar custom, lembrar posição
│       │       └── autostart.rs
│       └── public/
│
├── registry/                     # catálogo oficial de manifestos (pode virar repo separado "forgebox-registry")
│   ├── index.yaml                # lista de tools, versão do schema, assinatura
│   ├── tools/
│   │   ├── php.yaml
│   │   ├── apache.yaml
│   │   ├── nginx.yaml
│   │   ├── caddy.yaml
│   │   ├── frankenphp.yaml
│   │   ├── mysql.yaml
│   │   ├── mariadb.yaml
│   │   ├── postgresql.yaml
│   │   ├── mongodb.yaml
│   │   ├── memurai.yaml
│   │   ├── redis-windows.yaml
│   │   ├── memcached.yaml
│   │   ├── mailpit.yaml
│   │   ├── minio.yaml
│   │   ├── meilisearch.yaml
│   │   ├── nodejs.yaml
│   │   ├── bun.yaml
│   │   ├── python.yaml
│   │   ├── composer.yaml
│   │   ├── git.yaml
│   │   ├── cloudflared.yaml
│   │   ├── ngrok.yaml
│   │   ├── phpmyadmin.yaml
│   │   ├── adminer.yaml
│   │   ├── heidisql.yaml
│   │   └── xdebug.yaml           # kind: Extension (depende de php)
│   ├── stacks/
│   │   ├── laravel.yaml
│   │   ├── wordpress.yaml
│   │   ├── symfony.yaml
│   │   ├── cakephp.yaml
│   │   └── nextjs-php-api.yaml
│   └── schema/
│       ├── tool.v1.json          # gerados por `cargo xtask schema`
│       ├── extension.v1.json
│       ├── stack.v1.json
│       ├── project.v1.json
│       └── registry.v1.json      # index.yaml
│
├── installer/
│   ├── nsis/                     # hooks NSIS: PATH, exclusão do Defender (opt-in), associação forgebox://
│   ├── portable/                 # script que monta o zip portátil
│   └── assets/                   # banners, ícones do instalador
│
├── xtask/                        # cargo xtask: build-all, gen-ts (contratos → TS), schema, release, lint-registry
├── scripts/                      # ci.ps1 (CI local), dev: setup.ps1, run-daemon.ps1, smoke.ps1
├── tests/
│   ├── e2e/                      # testes de ponta a ponta contra daemon real (Windows runner)
│   └── fixtures/                 # zips mínimos fake de "tools" para testar providers/extract/layout
└── .github/
    └── workflows/                # ci.yml (fmt, clippy, build, test, deny, lint-registry, artefatos gerados, pnpm build), dependabot.yml, PULL_REQUEST_TEMPLATE.md, release.yml (build, sign, latest.json)
```

## Regras de organização

1. **Nada de lógica de tool específica fora de `registry/` e `forge-render/templates/`.** Se um agente sentir vontade de escrever `if tool.id == "mysql"` em um crate, a resposta é estender o schema do manifesto.
2. **Toda chamada Win32 fica em `forge-core/src/platform/windows/`** atrás de um trait. Outros crates só veem o trait.
3. **GUI e CLI nunca importam crates de domínio.** Só `forge-ipc`. O xtask `gen-ts` gera os tipos TypeScript a partir dos contratos Rust (via `ts-rs` ou `specta`).
4. **Templates são dados**, embutidos no binário por `include_dir!`, mas o usuário pode sobrescrever qualquer um colocando o mesmo caminho em `<root>/etc/templates/` (ver [08-release-layout.md](08-release-layout.md)).
5. **Migrations SQLite são numeradas** (`0001_init.sql`...) e nunca editadas depois de publicadas.
6. **Cada crate tem `README.md` de 10 linhas** dizendo o que é, o que não é e quem depende dele.
7. **Testes:** unitários ao lado do código; integração em `crates/<x>/tests/`; e2e em `tests/e2e/` usando um `ForgeRoot` temporário e fixtures de tools fake (um `php.exe` fake que responde FastCGI, por exemplo).
