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
│   │       ├── runtime.rs        # build_runtime (max(4, núcleos) workers, 2 × núcleos blocking, threads nomeadas); feature `console`;
│   │       │                     #   spawn_blocking_timeout (chamada bloqueante com prazo)
│   │       ├── store/            # SQLite: migrations/*.sql (include_str), runner com backup, repos por agregado (events.rs, tool_versions.rs...),
│   │       │                     #   writer.rs (thread de escrita, lotes com SAVEPOINT), reader.rs (pool query_only p/ spawn_blocking),
│   │       │                     #   ports.rs (PortRepo: reservas de porta)
│   │       ├── model/            # Tool, ToolVersion, Instance, Site, Proxy, App, Port, Certificate (hoje: port.rs —
│   │       │                     #   PortReservation, ReservationOwner, OwnerKind, PortState, PortHolder e o trait
│   │       │                     #   PortReservations: o alocador visto por quem não depende de forge-net, ex. forge-fcgi)
│   │       ├── config.rs         # forgebox.toml (preferências, faixas de portas nomeadas, [dns] extra_tlds/remote_zones)
│   │       ├── dns_name.rs       # sintaxe de nomes DNS (rótulos minúsculos), sufixos de uso local (test, localhost, home.arpa...)
│   │       ├── fs/               # case/ (docs/15, ADR-0024): status (flag, FS, build), convert_tree (recria a árvore em
│   │       │                     #   <dir>.cs-tmp, rename de arquivos/links, troca, verificação, rollback), recover, tree_flags,
│   │       │                     #   CaseConversions (job, progresso, fs.case_sensitivity_*), fold_case, messages.rs (pt-BR)
│   │       ├── helper/           # protocolo do forgebox-helper (ADR-0022): op.rs (HelperOp allowlist + HelperPolicy/validate),
│   │       │                     #   protocol.rs (HelperRequest/HelperResponse v1, arquivos de troca em state/locks),
│   │       │                     #   hosts.rs (bloco gerenciado do hosts), client.rs (HelperClient, FakeElevation)
│   │       └── platform/         # traits (LinkFs, CaseSensitiveFs, CodeSign, FileAcl, TrustStore) + current();
│   │                             #   trust_store.rs: TrustStore (raízes por CertThumbprint SHA-1, CurrentUser/LocalMachine,
│   │                             #   verify_server: cadeia pelo verificador do SO com raiz exclusiva, revogação opcional);
│   │                             #   dns_policy.rs: DnsPolicy (regras NRPT, hosts, flush do cache; mudanças via helper), NrptRule;
│   │                             #   elevation.rs: Elevation (forgebox-helper elevado); net_adapters.rs: NetAdapters (adaptadores + DNS);
│   │                             #   process.rs: ProcessControl/ProcessGroup (Job Objects, spawn, CTRL_BREAK, WM_CLOSE);
│   │                             #   process_stats.rs: ProcessStats (tempo de CPU, working set, commit privado, handles por PID);
│   │                             #   probe.rs: run_probe (comando de uma execução, saída capturada, timeout, sem janela);
│   │                             #   net_table.rs: PortTable (listeners TCP v4/v6 com PID/exe, conexões ESTABLISHED por porta,
│   │                             #   connections(portas): todas as conexões das portas, TIME_WAIT incluído, as duas pontas;
│   │                             #   udp_endpoints: sockets UDP v4/v6 com PID/exe);
│   │                             #   runtime_deps.rs: RuntimeDeps, RuntimeDep, VcRedistFlavor (vc15/vs16/vs17/vs18 → versão mínima)
│   │                             #   impl windows/: link_fs.rs (crate junction), case_sensitive_fs.rs (NtSetInformationFile),
│   │                             #   authenticode.rs (WinVerifyTrust + catálogos, publisher via CryptQueryObject),
│   │                             #   file_acl.rs (DACL só do usuário), pipe_security.rs (descritor do pipe da API, checagem do dono),
│   │                             #   trust_store.rs (CertOpenStore SYSTEM_REGISTRY, CertAdd/Find/Delete por SHA-1,
│   │                             #   motor de cadeia com hExclusiveRoot + CERT_CHAIN_POLICY_SSL),
│   │                             #   process.rs (Job Objects, CreateProcessW suspenso, pipes, RegisterWaitForSingleObject),
│   │                             #   process_stats.rs (GetProcessTimes, GetProcessMemoryInfo/PROCESS_MEMORY_COUNTERS_EX, GetProcessHandleCount),
│   │                             #   net_table.rs (GetExtendedTcpTable/GetExtendedUdpTable + QueryFullProcessImageNameW),
│   │                             #   elevation.rs (ShellExecuteExW runas + arquivos de troca com DACL usuário+Administrators),
│   │                             #   dns_policy.rs (NRPT no registro, DataBasePath do hosts, DnsFlushResolverCache, ops via helper),
│   │                             #   net_adapters.rs (GetAdaptersAddresses),
│   │                             #   vcredist.rs (registro VC\Runtimes\x64 + versão de vcruntime140.dll/msvcp140.dll),
│   │                             #   console_ctrl.rs (forgebox-ctrl, ADR-0016), window_close.rs (EnumWindows + WM_CLOSE)
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
│   │       ├── index.rs          # index.yaml: assinatura minisign, sha256 dos manifestos, política de terceiros
│   │       ├── cache.rs          # state/registry/<nome>/ (escrita atômica, reverificado a cada leitura)
│   │       ├── source.rs         # fontes: git (comando `git` da máquina), http (index.yaml + relativos), dir local
│   │       ├── service.rs        # `Registry`: refresh(name) -> JobId, load, find, search, list; metadados em `registries`
│   │       ├── messages.rs       # strings pt-BR (badges "não verificado", "terceiro")
│   │       ├── search.rs         # busca por id/nome/tag/categoria, ranking
│   │       └── discover.rs       # discover() + import_discovered(): pastas soltas em bin/ reconhecidas só pelos manifestos (spec.discover)
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
│   │   ├── src/
│   │   │   ├── job.rs            # JobTree: Job Object raiz e por instância; nested() = Job de um pool (workers aninhados)
│   │   │   ├── process.rs        # SpawnSpec: env mínimo + PATH determinístico, stdio Capture|Discard|File, console oculto
│   │   │   ├── logs.rs           # bombas de pipe → .out.log (rotação 10 MiB × 5, backpressure) + broadcast drop-oldest
│   │   │   ├── stop.rs           # cascata: stop.command → CTRL_BREAK → WM_CLOSE → TerminateJobObject
│   │   │   ├── events.rs         # ServiceEventPayload: payloads service.* (v1), mesma forma de forge_ipc ServicePayload
│   │   │   ├── health.rs         # probes tcp/http/command/pid, limiares, start_timeout, intervalo com jitter
│   │   │   ├── policy.rs         # restart: never|on-failure{max, window, backoff}|always; RestartTracker (puro)
│   │   │   ├── metrics.rs        # sampler por Job da instância: CPU % (Δcpu/Δparede/CPUs lógicas), working set, handles
│   │   │   ├── precheck.rs       # pré-checagens antes do spawn: runtime (VC++), depends_on (DependencyLookup injetada),
│   │   │   │                     #   portas (PortTable), service.check (Job próprio, saída capturada, timeout)
│   │   │   ├── service/          # mod.rs: InstanceActor, InstanceHandle, Supervisor;
│   │   │   │                     #   state.rs: máquina de estados pura (next_state), FailureReason, StopCause
│   │   │   └── bin/forgebox-ctrl.rs  # auxiliar que entrega CTRL_BREAK ao console de um serviço (ADR-0016)
│   │   └── tests/
│   │       └── fake_service/     # projeto cargo independente (sem deps) compilado pelos testes: serviço falso
│   ├── forge-fcgi/
│   │   ├── src/
│   │   │   ├── pool.rs           # ator PhpPool + PoolHandle: PoolSpec (scaling, recycle, timing), portas via PortReservations,
│   │   │   │                     #   amostragem da tabela TCP, recycle proativo, scaling, drenagem, Start/standby/Promote/Drain/Stop
│   │   │   ├── worker.rs         # um php-cgi.exe: InstanceSpec (env PHPRC/PHP_INI_SCAN_DIR/PHP_FCGI_MAX_REQUESTS/FORGEBOX_SITE),
│   │   │   │                     #   launch (porta, spawn, warmup), ConnTracker (requests estimadas), retire
│   │   │   ├── probe.rs          # cliente FastCGI mínimo: FCGI_GET_VALUES, GET, health.php (HEALTH_SCRIPT)
│   │   │   ├── upstream.rs       # UpstreamModel {name, backends, policy}, UpstreamListener (re-render + reload pelo daemon)
│   │   │   ├── events.rs         # payloads pool.scaled / pool.worker.recycled (forma de forge_ipc::api::events)
│   │   │   └── swap.rs           # blue/green entre pools (standby → listener → promote → drain), Switcher por VersionId
│   │   └── tests/
│   │       └── fake_php_cgi/     # projeto cargo independente (sem deps) compilado pelos testes: php-cgi falso (FastCGI)
│   ├── forge-render/
│   │   ├── templates/            # *.j2 embutidos (include_dir): apache/, nginx/, caddy/, php/, mysql/, mariadb/, postgresql/, redis/, mailpit/
│   │   └── src/
│   │       ├── engine.rs         # minijinja env, filtros (win_path, posix_path, quote...)
│   │       ├── managed.rs        # blocos "# forgebox:begin/end", preservação de edições do usuário
│   │       ├── drift.rs          # hash do render vs arquivo em disco
│   │       └── targets/          # um módulo por tool conhecida: o que renderiza e para onde
│   ├── forge-sites/
│   │   └── src/
│   │       ├── site.rs           # SiteDraft, validação de host, aliases (Site + SiteRepo ficam em forge-core/store/sites.rs)
│   │       ├── service.rs        # CRUD de sites + eventos site.created/updated/removed
│   │       ├── drivers/          # trait Driver + mod.rs (detecção/desempate), php.rs (laravel, symfony, wordpress, cakephp, drupal, magento, generic-php), web.rs (static, spa)
│   │       ├── parked.rs         # watcher (notify) em diretórios "parked" → sites implícitos (ignora *.cs-tmp/*.cs-old)
│   │       ├── lint_case/        # Case Mismatch Finder (docs/15 §6): mod.rs (varredura, relatório = CaseLintReportDto),
│   │       │                     #   refs.rs (extração por regex por tipo de arquivo, manifestos, PSR-4), resolve.rs (listagens
│   │       │                     #   exatas, candidatos .js/index/_parcial/.blade.php), fix.rs (renomear/editar, auto, prévia)
│   │       ├── case_parity.rs    # estado Aa/aA por site (pasta do projeto), reverify no boot, jobs site_case e lint_case
│   │       ├── parity.rs         # git_parity_init: core.ignorecase/autocrlf=false, .gitattributes, .editorconfig (forge new)
│   │       ├── vhost.rs          # gera modelo de vhost (independente de servidor) → forge-render
│   │       ├── proxy.rs          # sites proxy (Node/Next/Vite...), websocket, headers
│   │       ├── apps.rs           # Procfile / processes: web, worker, scheduler, com $PORT
│   │       └── project_file.rs   # leitura de forgebox.yaml do projeto (kind: Project)
│   ├── forge-net/
│   │   ├── examples/dns_serve.rs # teste manual do resolvedor (docs/06 §5.5)
│   │   └── src/
│   │       ├── ports.rs          # PortAllocator: faixas do forgebox.toml, reservas (PortRepo), bind de teste, conflicts()
│   │       ├── dns/              # zones.rs (ZoneSet: TLDs locais, zonas remotas, NXDOMAIN fora; sem upstream),
│   │       │                     #   server.rs (DnsServer: hickory UDP+TCP, ator dns-server, fallback 127.0.0.53:53),
│   │       │                     #   client.rs (consulta direta UDP/TCP), diagnose.rs (DnsDiagnostics → DnsDiagnosis + hints)
│   │       ├── policy.rs         # DnsPolicyManager: NRPT via helper, hosts fallback, flush, eventos dns.policy_applied/helper.op
│   │       ├── messages.rs       # textos pt-BR das dicas de diagnóstico
│   │       └── firewall.rs       # regras via helper (netsh/API) só se o usuário quiser acesso LAN
│   ├── forge-tls/                # ADR-0023
│   │   ├── examples/tls_demo.rs  # teste manual: CA + folhas em uma raiz, HTTPS de teste + responder de CRL (docs/06 §6.4)
│   │   └── src/
│   │       ├── ca.rs             # CA ECDSA P-256, 10 anos, pathLen 0, name constraints críticas (TLDs de uso local,
│   │       │                     #   localhost, 127.0.0.0/8, ::1/128); certs/ca/ com chave só do usuário; recuperação de escrita
│   │       ├── leaf.rs           # cert por site: SAN host + *.host + aliases, 825 dias, serverAuth, CRL DP; certs/sites/
│   │       ├── crl.rs            # CRL vazia assinada pela CA + responder HTTP mínimo em 127.0.0.1 (Schannel/curl.exe)
│   │       ├── cert.rs           # PEM, leitura de volta (x509-parser), thumbprints, escrita atômica de par cert/chave
│   │       ├── trust.rs          # livro certs/ca/trust.json (raízes adicionadas por thumbprint) + instruções Node/Firefox/Python/Java
│   │       ├── manager.rs        # ator `tls` (TlsManager): issue, renew, rotate (duas fases), status, install/uninstall,
│   │       │                     #   verificação diária, eventos tls.* (forma de TlsPayload) e helper.op
│   │       └── messages.rs       # textos pt-BR (instruções, avisos)
│   ├── forge-ipc/                # protocolo: ADR-0015
│   │   └── src/
│   │       ├── api/              # contratos: um módulo de DTOs por grupo do docs/09 (daemon, tools, services, logs, php, sites,
│   │       │                     #   db, net, tls, mail, shell, fs_parity, jobs, stats, remote, registry, system, fs, events) +
│   │       │                     #   common.rs (Role, MethodInfo, ApiError, SiteRef...), methods.rs (tabela única METHODS → marcadores,
│   │       │                     #   Client, methods.ts), contracts.rs (export TS: generated.ts + methods.ts)
│   │       ├── rpc.rs            # frames JSON-RPC 2.0 (request_id obrigatório, códigos de erro)
│   │       ├── auth.rs           # state/ipc.token (ACL do usuário), Authenticator (system.hello)
│   │       ├── transport/        # trait Transport/Listener; framing NDJSON; named_pipe (windows), unix (cfg unix), memory, websocket (placeholder M6)
│   │       ├── stream_queue.rs   # fila drop-oldest com marcadores de lag (servidor e cliente)
│   │       ├── server/           # dispatcher (Router/Ctx), conexão (leitor/escritor/outbox), idempotência por request_id,
│   │       │                     #   events.rs (subscribe + resume_from: anel do EventBus + tabela events), fs.rs (FsScope), builtins
│   │       └── client/           # Client tipado (métodos gerados da tabela), retentativa com mesmo request_id, Subscription com reassinatura
│   ├── forge-daemon/             # bin forgeboxd.exe: main.rs, boot.rs, scheduler.rs, tray-less
│   ├── forge-cli/                # bin forge.exe: commands/*.rs (um por grupo), output.rs (table|json|plain)
│   ├── forge-helper/             # bin forgebox-helper.exe: executa as ops de forge_core::helper (ADR-0022), manifest UAC, logging
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
│       │   ├── ipc/              # generated.ts (DTOs) + methods.ts (tabela de métodos, ApiMethods), gerados por `xtask gen-ts`
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
│   ├── index.yaml                # lista de tools, versão do schema, assinatura minisign
│   ├── keys/dev.pub              # chave pública de desenvolvimento (a secreta fica fora do repo)
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
├── xtask/                        # cargo xtask: build-all, gen-ts (contratos → TS), schema, release, lint-registry, registry-index, registry-sign, registry-keygen
├── scripts/                      # ci.ps1 (CI local), dev: setup.ps1, run-daemon.ps1, smoke.ps1
├── tests/
│   ├── e2e/                      # testes de ponta a ponta contra daemon real (Windows runner)
│   └── fixtures/                 # zips mínimos fake de "tools" para testar providers/extract/layout;
│       │                         #   registry/ (assinado com a chave dev, 2 tools + 1 stack) e registry-bad-sha/ (sha errado no índice)
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
