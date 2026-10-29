# 02 — Estrutura do repositório

Monorepo Cargo workspace + app Tauri + registry de manifestos. Tudo que um agente precisa saber para colocar um arquivo no lugar certo.

```
Forgebox/
├── Cargo.toml                    # workspace: members = ["crates/*", "xtask", "apps/desktop/src-tauri"]; [profile.release] com LTO e opt-level="s" só para o shell Tauri
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
│   │       ├── paths.rs          # ForgeRoot + todos os caminhos (bin/, etc/, data/...), discover(), init() com flag CS;
│   │       │                     #   daemon_lock_file() (state/locks/forgeboxd.lock: evidência de saída limpa, ADR-0034)
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
│   │       │                     #   ports.rs (PortRepo: reservas de porta), update_history.rs (UpdateHistoryRepo: updates e rollbacks),
│   │       │                     #   instances.rs (InstanceRepo: instâncias de serviço, desired_state, settings)
│   │       ├── model/            # Tool, ToolVersion, Instance, Site, Proxy, App, Port, Certificate (hoje: port.rs —
│   │       │                     #   PortReservation, ReservationOwner, OwnerKind, PortState, PortHolder e o trait
│   │       │                     #   PortReservations: o alocador visto por quem não depende de forge-net, ex. forge-fcgi)
│   │       ├── config.rs         # forgebox.toml (preferências, faixas de portas nomeadas, [dns] enabled/extra_tlds/remote_zones)
│   │       ├── dns_name.rs       # sintaxe de nomes DNS (rótulos minúsculos), sufixos de uso local (test, localhost, home.arpa...)
│   │       ├── fs/               # case/ (docs/15, ADR-0026): status (flag, FS, build), convert_tree (recria a árvore em
│   │       │                     #   <dir>.cs-tmp, rename de arquivos/links, troca, verificação, rollback), recover, tree_flags,
│   │       │                     #   CaseConversions (job, progresso, fs.case_sensitivity_*), fold_case, messages.rs (pt-BR)
│   │       ├── helper/           # protocolo do forgebox-helper (ADR-0022): op.rs (HelperOp allowlist + HelperPolicy/validate),
│   │       │                     #   protocol.rs (HelperRequest/HelperResponse v1, arquivos de troca em state/locks),
│   │       │                     #   hosts.rs (bloco gerenciado do hosts; apply_hosts_block_bytes preserva bytes fora do bloco),
│   │       │                     #   client.rs (HelperClient, FakeElevation), trust.rs (CA aceita em LocalMachine\Root, ADR-0024)
│   │       └── platform/         # traits (LinkFs, CaseSensitiveFs, CodeSign, FileAcl, TrustStore, UserEnv) + current();
│   │                             #   appearance.rs: SystemAppearance + system_appearance() → AppearanceSnapshot (accent #RRGGBB,
│   │                             #   apps_light, taskbar_light, transparency, high_contrast, remote_session, os_build, is_win11;
│   │                             #   defaults documentados, stub fora do Windows); impl em windows/appearance.rs (HKCU Personalize/DWM,
│   │                             #   DwmGetColorizationColor, SPI_GETHIGHCONTRAST, SM_REMOTESESSION, RtlGetVersion). A GUI lê por
│   │                             #   forge_ipc::base::platform (sem Win32 em src-tauri); tests/win32_boundary.rs falha com Win32 fora
│   │                             #   de platform/windows (só código Rust: comentários e literais são ignorados);
│   │                             #   user_env.rs: UserEnv (PATH persistente do usuário), PathValue (lista crua, sem duplicatas),
│   │                             #   MemoryUserEnv (fake em memória); probe.rs também tem run_probe_with_env (ações run);
│   │                             #   trust_store.rs: TrustStore (raízes por CertThumbprint SHA-1, CurrentUser/LocalMachine,
│   │                             #   verify_server: cadeia pelo verificador do SO com raiz exclusiva, revogação opcional);
│   │                             #   privileged.rs: PrivilegedOps + privileged_ops() (mudanças de máquina do forgebox-helper:
│   │                             #   NRPT, hosts, LocalMachine\Root, firewall, Defender, instalador verificado; ADR-0024);
│   │                             #   secure_file.rs: open_no_follow/final_path (arquivos da raiz sem seguir links);
│   │                             #   dns_policy.rs: DnsPolicy (regras NRPT, hosts, flush do cache; mudanças via helper), NrptRule;
│   │                             #   elevation.rs: Elevation (forgebox-helper elevado); net_adapters.rs: NetAdapters (adaptadores + DNS);
│   │                             #   host.rs: mutex de instância única do daemon (InstanceMutex, daemon_mutex_name), NetworkCost
│   │                             #   (conexão medida), spawn_detached (daemon.restart); impl em windows/host.rs (CreateMutexW,
│   │                             #   INetworkCostManager);
│   │                             #   process.rs: ProcessControl/ProcessGroup (Job Objects, spawn, CTRL_BREAK, WM_CLOSE);
│   │                             #   process_stats.rs: ProcessStats (tempo de CPU, working set, commit privado, handles por PID),
│   │                             #   process_creation_time(pid) (início da medição de cold start da GUI);
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
│   │                             #   console_ctrl.rs (forgebox-ctrl, ADR-0016), window_close.rs (EnumWindows + WM_CLOSE),
│   │                             #   user_env.rs (HKCU\Environment\Path via RegQueryValueExW/RegSetValueExW, REG_EXPAND_SZ
│   │                             #   preservado, SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE "Environment")),
│   │                             #   privileged.rs (WindowsPrivilegedOps), com.rs (COM/VARIANT), wmi.rs (métodos estáticos WMI:
│   │                             #   PS_DnsClientNrptRule, MSFT_MpPreference), firewall.rs (INetFwPolicy2), cert_info.rs (parse de CA:
│   │                             #   Basic/Name Constraints; o store é o trust_store.rs), secure_file.rs (FILE_FLAG_OPEN_REPARSE_POINT, nlink), pe_resources.rs (manifest embutido)
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
│   │       └── discover.rs       # discover()/discover_in(bin) + import_discovered(): pastas soltas em bin/ reconhecidas só pelos manifestos (spec.discover)
│   ├── forge-provider/
│   │   ├── keys/                 # keyrings PGP embutidos (nginx.asc, nodejs.asc), fingerprints comentados e fixados por teste
│   │   └── src/
│   │       ├── provider/         # mod.rs (trait Provider, Release, ProviderRegistry::for_spec), fetch.rs (cache de metadados TTL+ETag),
│   │       │                     #   filter.rs (constraint/canal/variantes/ordem), checksum.rs, messages.rs (pt-BR) e uma impl por tipo:
│   │       │                     #   http, github_release, php_windows, apache_lounge, nginx_org, mysql, mariadb, postgresql_edb,
│   │       │                     #   nodejs_dist, curated.rs (releases: do manifesto); python_embed fica para depois
│   │       ├── download.rs       # fila (ator), paralelismo, resume (Range), progresso, mirrors; submódulos download/{transfer,limiter,progress,messages}.rs
│   │       ├── verify.rs         # tamanho, sha256/sha512, minisign, Authenticode (platform::CodeSign); verify/pgp.rs (rpgp, keyrings)
│   │       ├── extract.rs        # zip/7z/tar.gz/tar.xz/arquivo único; strip_root; staging em tmp/stage e move atômico;
│   │       │                     #   extract/{entry (nomes seguros), formats, install (strip, rename, cópia entre volumes)}.rs
│   │       ├── messages.rs       # textos pt-BR de verify/extract/layout e dos shims (até existir i18n)
│   │       ├── layout.rs         # Layout: install_from_archive -> JobId (verify → staging → extract → post_install →
│   │       │                     #   move → tool_versions → current → shims), use_version/current_version, ensure_shims/path;
│   │       │                     #   layout/{actions (post_install, TemplateRenderer, install.verify), current (seleção de versão),
│   │       │                     #   shims (.cmd + forgebox-project.ps1), path (PATH do usuário via UserEnv), vars}.rs
│   │       └── update.rs         # UpdateEngine (ator update-engine): check/apply/rollback -> JobId, ganchos do agendador;
│   │                             #   update/{policy (política efetiva, linha, versão ativa, plano), schedule (next_check_at:
│   │                             #   intervalo+jitter, quiet_hours, conexão medida), migrate (copy_dir), deps (traits
│   │                             #   ToolCatalog, ReleaseSource, ArchiveFetcher/DownloadFetcher, PoolSwitcher, MeteredProbe)}.rs
│   ├── forge-supervisor/
│   │   ├── src/
│   │   │   ├── job.rs            # JobTree: Job Object raiz e por instância; nested() = Job de um pool (workers aninhados)
│   │   │   ├── process.rs        # SpawnSpec: env mínimo + PATH determinístico, stdio Capture|Discard|File, console oculto
│   │   │   ├── logs.rs           # bombas de pipe → hub por instância (seq continua os arquivos) → .out.log (rotação 10 MiB × 5,
│   │   │   │                     #   backpressure) + broadcast drop-oldest
│   │   │   ├── logs/set.rs       # domínio de seq dos logs (ADR-0046): LogSet Rotated (+ sidecar .seq)/Daily, snapshot, tail,
│   │   │   │                     #   range, Follower (polling que atravessa rotação sem repetir nem recuar)
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
│   │   ├── templates/            # *.j2 embutidos (include_dir): apache/, nginx/, caddy/, php/, mysql/, mariadb/, postgresql/, redis/, mailpit/, pages/ (HTML servido pelo daemon: starting.html.j2)
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
│   │       ├── import/           # `forge import laragon` (ADR-0031): laragon.rs (LaragonImporter::scan → ImportPlan, só leitura), apply.rs (apply: tools
│   │       │                     #   por referência/cópia, instâncias de banco, parked, sites, dry-run), plan.rs (ImportPlan/ImportOptions/ImportReport),
│   │       │                     #   conf.rs (Apache `auto.*.conf` com Define/${VAR}, Nginx server{}), ini.rs (usr/laragon.ini), messages.rs (pt-BR)
│   │       ├── lint_case/        # Case Mismatch Finder (docs/15 §6): mod.rs (varredura, relatório = CaseLintReportDto),
│   │       │                     #   refs.rs (extração por regex por tipo de arquivo, manifestos, PSR-4), resolve.rs (listagens
│   │       │                     #   exatas, candidatos .js/index/_parcial/.blade.php), fix.rs (renomear/editar, auto, prévia)
│   │       ├── case_parity.rs    # estado Aa/aA por site (pasta do projeto), reverify no boot, jobs site_case e lint_case
│   │       ├── parity.rs         # git_parity_init: core.ignorecase/autocrlf=false, .gitattributes, .editorconfig (forge new)
│   │       ├── vhost.rs          # gera modelo de vhost (independente de servidor) → forge-render
│   │       ├── proxy.rs          # sites proxy (Node/Next/Vite...): ProxyOptions (websocket, headers), ProxyTarget, proxy_vhost → ProxyModel
│   │       ├── apps/             # Procfile / processes (ADR-0028): command.rs ($PORT, argumentos MSVCRT, direto vs `cmd.exe /d /c %FORGEBOX_COMMAND%`),
│   │       │                     #   runtime.rs (RuntimeCatalog: runtime tool@série, version_files, config.env do manifesto), spec.rs (AppSite → AppPlan →
│   │       │                     #   AppSpec → InstanceSpec), manager.rs (ator `apps`: AppManager, Job por site, on-visit, página /_forgebox/starting)
│   │       └── project_file.rs   # leitura de forgebox.yaml do projeto (kind: Project)
│   ├── forge-net/
│   │   ├── examples/dns_serve.rs # teste manual do resolvedor (docs/06 §5.5)
│   │   └── src/
│   │       ├── ports.rs          # PortAllocator: faixas do forgebox.toml, reservas (PortRepo), bind de teste, conflicts()
│   │       ├── dns/              # zones.rs (ZoneSet: TLDs locais, zonas remotas, NXDOMAIN fora; sem upstream),
│   │       │                     #   server.rs (DnsServer: hickory UDP+TCP, ator dns-server, fallback 127.0.0.53:53),
│   │       │                     #   client.rs (consulta direta UDP/TCP), diagnose.rs (DnsDiagnostics → DnsDiagnosis + hints)
│   │       ├── policy.rs         # DnsPolicyManager: NRPT via helper verificada por sonda, hosts fallback persistido em state/dns-policy.json (ADR-0033), fila de vez enable/disable/sync, flush, eventos dns.policy_applied/helper.op
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
│   │       ├── bundle.rs         # certs/ca-bundle.pem: CA local + raízes confiadas pelo Windows (PHP/curl/Composer, docs/06 §6.5)
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
│   │       └── client/           # mod.rs: Client tipado (métodos gerados da tabela) + call_raw/subscribe_raw (pontes JSON da GUI),
│   │                             #   retentativa com mesmo request_id, ConnectionState em watch (Idle/Connecting/Connected/Reconnecting/
│   │                             #   Closed), ping com RTT, CredentialsProvider (token relido a cada conexão), keep_connected;
│   │                             #   conn.rs (conexão: leitor/escritor, hello); subscription.rs (cursor, piso, dedupe por seq;
│   │                             #   a manutenção não reassina o que ainda está assinando e restaura logo após uma conexão
│   │                             #   aberta por chamada, ADR-0034);
│   │                             #   local.rs (daemon local: try_connect Down/Other, find_daemon_exe, spawn_daemon vigiado,
│   │                             #   wait_ready com saída 4 = outro daemon subindo, connect_or_start; usado por forge e Forgebox.exe)
│   ├── forge-daemon/             # bin forgeboxd.exe (lib forge_daemon + main.rs), tray-less; ADR-0029
│   │   ├── src/                  # args.rs (clap), instance_lock.rs (mutex + state/locks/forgeboxd.lock), logging.rs (JSON lines
│   │   │                         #   diários em logs/forgeboxd.log.<data>), boot.rs (docs/01 §5.1), daemon.rs (componentes),
│   │   │                         #   event_log.rs (eventos → tabela events), catalog.rs (manifestos + versões instaladas),
│   │   │                         #   vars.rs (variáveis de execução dos manifestos), services.rs (instâncias ← service.daemon),
│   │   │                         #   pools.rs (pools ← service.pool, por VersionId), web.rs (sites → pools → servidores web,
│   │   │                         #   UpstreamListener), render.rs (alvos do forge-render, TemplateRenderer do layout),
│   │   │                         #   install.rs (tools.install: provider → download → layout), scheduler.rs (tarefas periódicas
│   │   │                         #   e reações), shutdown.rs (parada em cascata), messages.rs (pt-BR), api/ (um módulo por grupo,
│   │   │                         #   convert.rs domínio → DTO, not_implemented.rs com a tarefa do roadmap)
│   │   └── tests/                # processo real em raiz temporária: lifecycle.rs, up_down_loop.rs (aceite 100×);
│   │                             #   #[ignore] com binários oficiais (common/online.rs): php_pool_online.rs,
│   │                             #   web_nginx_php_online.rs, mailpit_service_online.rs; concurrency.rs (orçamentos de
│   │                             #   docs/03 §7.6 com fixtures/fake_mailpit.rs: serviço travado, cliente lento, download + start)
│   ├── forge-cli/                # bin forge.exe (lib forge_cli + main.rs): cli.rs (árvore clap com todos os comandos de docs/09),
│   │   │                         #   commands/*.rs (um por grupo), context.rs (flags globais + cliente), connect.rs (mensagens do daemon automático
│   │   │                         #   de forge_ipc::client::local),
│   │   │                         #   jobs.rs (acompanha jobs: eventos + poll, barras), output.rs (tabelas, cores, --json), render.rs
│   │   │                         #   (DTO → texto), messages.rs (pt-BR/en-US), error.rs (códigos → saída), remotes.rs, open.rs, parse.rs
│   │   └── tests/                # processo real contra forgeboxd real em raiz temporária: commands.rs (tabela de TODOS os comandos,
│   │                             #   --json e humano), output.rs, exit_codes.rs, lifecycle.rs (daemon automático), misc.rs; online.rs (#[ignore])
│   ├── forge-helper/             # forgebox-helper.exe: executa as ops de forge_core::helper (ADR-0022/0024); lib + bin (test = false)
│   │   ├── build.rs              # manifest requireAdministrator embutido (crate embed-manifest)
│   │   ├── src/                  # lib.rs (Helper: validação léxica + canônica, elevação, run), args.rs, exchange.rs (arquivos de
│   │   │                         #   troca), ops.rs (cada operação: plano/--dry-run e execução), log.rs (logs/helper.log), messages.rs
│   │   └── tests/                # cli.rs (binário real com __COMPAT_LAYER=RunAsInvoker, --dry-run), manifest.rs (nível de execução)
│   └── forge-mcp/                # bin forge-mcp.exe: expõe tools MCP (list_sites, create_site, logs, ...)
│
├── apps/
│   └── desktop/                  # Itens marcados (M2-E1-Tn) ainda não existem: chegam na tarefa indicada.
│       ├── package.json          # pnpm@10.7.1, node >= 24; react 19, vite 8, tailwind 4, vitest, eslint 9, prettier, @tauri-apps/cli
│       │                         #   (radix, motion e afins entram com as tarefas que os usam; zustand e @tanstack/react-query: M2-E1-T9)
│       ├── pnpm-lock.yaml
│       ├── index.html
│       ├── vite.config.ts        # porta 1420 (devUrl do Tauri) + config do vitest (jsdom)
│       ├── tsconfig.json         # strict; TypeScript 6.0.x (a faixa suportada pelo typescript-eslint)
│       ├── eslint.config.js      # ESLint 9 flat: typescript-eslint strict, react-hooks, jsx-a11y
│       ├── .prettierrc.json / .prettierignore / .gitignore
│       ├── README.md             # runbook: Node 24, NODE_OPTIONS, comandos, o que roda sem Node
│       ├── playwright.config.ts / playwright.real.config.ts  # M2-E1-T19 (ADR-0036): e2e web (Edge, build `qa` + vite preview, porta FORGE_QA_PORT, padrão 4174, sem reuso) e e2e da janela real (CDP)
│       ├── e2e/                  # fixtures.ts (emulação, axe, teclado, CLS), *.spec.ts por cenário; real/app.spec.ts (Forgebox.exe via CDP, só por scripts/e2e-desktop.ps1)
│       ├── qa/                   # fixtures/scenarios/ (biblioteca tipada com `satisfies` contra generated.ts: empty, populated, heavy...); out/ (gitignored)
│       ├── scripts/              # perf-startup.mjs (pnpm perf:startup: partida a frio, JSON), qa-root.mjs (raiz portátil temporária, killTree)
│       ├── dist-qa/              # saída de `vite build --mode qa` (gitignored; nunca sobre dist/)
│       ├── src/
│       │   ├── main.tsx / App.tsx / index.css   # casca placeholder; Tailwind com `source("./")` (não varre src-tauri/target)
│       │   ├── App.test.tsx / test/setup.ts     # vitest + Testing Library
│       │   ├── qa/               # (M2-E1-T19) bootstrap.ts (modo `qa`: ?scenario, theme, vibrancy, motion, locale, latency), pseudo-locale.ts; fora do build de produção (sentinela)
│       │   ├── app/              # appearance.ts (M2-E1-T7: data-theme/data-vibrancy/data-effect/data-contrast em <html>,
│       │   │                     #   boot por window.__FORGEBOX_SHELL__ + evento forge://appearance); shell/ (M2-E1-T7: Titlebar.tsx,
│       │   │                     #   WindowControls.tsx, windowApi.ts = janela Tauri lazy + `ready` após duplo rAF);
│       │   │                     #   (M2-E1-T10) roteamento (tanstack-router), sidebar, command palette
│       │   ├── design/           # (M2-E1-T6, T11) tokens.css, themes (light/dark/vibrancy), primitives/ (Button, Card, Switch...)
│       │   ├── features/         # (M2-E2/E3) dashboard/, services/, sites/, php/, databases/, mail/, logs/, catalog/, terminal/, settings/
│       │   ├── ipc/              # generated.ts (DTOs) + methods.ts (tabela de métodos, ApiMethods), gerados por `xtask gen-ts`;
│       │   │                     #   ponte (ADR-0034): bridge.ts (interface Bridge, ContextStateDto, StreamFrame), tauriBridge.ts
│       │   │                     #   (comandos ipc_* + forge://connection, ack por quadro), api.ts (ForgeApi: call<M>, job<M> →
│       │   │                     #   {job, done}, subscribe<M>; ULID por intenção), errors.ts (ForgeApiError, CLIENT_ERROR_CODES),
│       │   │                     #   fakeBridge.ts (o fake único de vitest/Playwright: cenários, jobs, streams, quedas),
│       │   │                     #   fixtures/ (bridge-dtos.json conferido pelo Rust; not-implemented.json conferido contra o daemon),
│       │   │                     #   cli.ts (renderCli: cli_template → comando `forge`, golden de forge-ipc/tests/fixtures/cli_templates.json),
│       │   │                     #   describeError.ts (ApiError/FailureReasonDto → causa i18n `errors:*` + ações com o comando `forge`)
│       │   ├── state/            # camada de dados (M2-E1-T9, ADR-0034 adendo): layer.ts (StateLayer: um ContextRuntime por contexto,
│       │   │                     #   QueryClient compartilhado), runtime.ts (stores + stream + snapshots + métricas + fila + RTT),
│       │   │                     #   stores.ts (stores Zustand por assunto, Draft copy-on-write, reconcile), reducers.ts (EVENT_REDUCERS:
│       │   │                     #   um reducer ou ignore por EventKind), effects.ts (JOB_EFFECTS: snapshots e queries a recarregar quando
│       │   │                     #   um job ou uma chamada mutável termina), frame.ts (lote por rAF, timer oculto, teto), eventStream.ts
│       │   │                     #   (events.subscribe: lagged/reset/end), snapshots.ts (SNAPSHOTS + resync com debounce), queries.ts
│       │   │                     #   (chaves [contexto, método, params], EVENT_INVALIDATIONS), jobs.ts (watchJob: poll de 1 s), metrics.ts
│       │   │                     #   (MetricsPoller: services.status a cada 2 s com consumidor visível e focado, anel de 30, req/s),
│       │   │                     #   capabilities.ts (capabilityOf), reconnectQueue.ts (mutáveis do usuário por até 30 s), page.ts
│       │   │                     #   (visível/focado), react.tsx (ForgeStateProvider + hooks: useInstance, useJob, useMetricsPoll,
│       │   │                     #   useStatusTotals, useCapabilities, useForgeQuery...), testFixtures.ts (só testes)
│       │   └── i18n/             # (M2-E1-T17) index.ts, format.ts, pseudo.ts, {pt-BR,en-US}/<ns>.json (tray/notifications também embutidos em src-tauri/src/i18n.rs)
│       ├── src-tauri/            # crate `forgebox-desktop` (membro do workspace; depende só de forge-ipc, guarda: `cargo xtask arch-guard`)
│       │   ├── Cargo.toml        # bin `Forgebox` (mainBinaryName), lib `forgebox_desktop`; feature `custom-protocol` embute dist
│       │   ├── build.rs          # tauri_build::build(); sem a feature, modo dev: compila sem Node e sem apps/desktop/dist
│       │   ├── tauri.conf.json   # janela `main`, CSP estrita sem origens remotas, identifier dev.forgebox.app (vibrancy/updater/bundle: tarefas seguintes)
│       │   ├── capabilities/     # main.json: permissões Tauri 2 só da janela `main`
│       │   ├── app-icon.png      # fonte dos ícones: `pnpm tauri icon src-tauri/app-icon.png`
│       │   ├── tests/            # bridge.rs (Server forge-ipc em memória: validação, retry com o mesmo request_id, 10 000
│       │   │                     #   eventos em quadros, lag, política com perdas reais), bridge_daemon.rs (forgeboxd real em
│       │   │                     #   raiz portátil temporária: start, kill → restart, daemon.stop → parado)
│       │   ├── icons/
│       │   └── src/
│       │       ├── main.rs / lib.rs  # lib.rs: Builder (Bridge gerenciado, on_page_load fecha streams da página) +
│       │       │                     #   teste que exige `async` em todo #[tauri::command]
│       │       ├── commands.rs   # app_info (fatos do build); nunca lógica de domínio aqui
│       │       ├── bridge/       # ponte IPC (ADR-0034): mod.rs (Bridge: call/subscribe/state/start, validação contra METHODS),
│       │       │                 #   commands.rs (ipc_state|call|subscribe|ack|unsubscribe|start, evento forge://connection),
│       │       │                 #   contexts.rs (ator Contexts: um Client por contexto, LocalConnector = discover +
│       │       │                 #   connect_or_start, vigia de daemon.shutdown, evidência do arquivo de trava), policy.rs
│       │       │                 #   (política pura de auto-start), streams.rs (bomba: quadros ≤ 16 ms/≤ 256, janela de 4
│       │       │                 #   confirmados, lagged/reset/end; registro de streams), errors.rs (ClientError → ApiError)
│       │       ├── tray.rs       # (M2-E1-T15) ícone/menu de bandeja, estados, ações rápidas
│       │       ├── window.rs     # (M2-E1-T7, ADR-0035) janela escondida até `ready`, choose_effect (Mica/Acrylic/opaco)
│       │       │                 #   aplicado por window-vibrancy, reavaliação em tema/foco/prefs, fechar → bandeja,
│       │       │                 #   segunda instância foca (tauri-plugin-single-instance), comandos ready/appearance_get
│       │       ├── window_state.rs # posição/tamanho/maximizada por monitor (window-state.json), mínimo min(960×600, área)
│       │       ├── data_dir.rs   # dados da GUI: FORGE_GUI_DATA_DIR → <raiz>/state/gui (portátil) → %LOCALAPPDATA%\Forgebox\gui;
│       │       │                 #   root_initialized (state/ só com gui/ não conta)
│       │       ├── prefs.rs      # prefs.json (theme, vibrancy, win10_acrylic, close_to_tray + chaves de outras features),
│       │       │                 #   comandos gui_prefs_get/gui_prefs_set
│       │       ├── perf.rs       # criação do processo → `ready` (perf_startup, `--perf-startup` imprime JSON e sai)
│       │       └── autostart.rs  # (M2-E1-T18)
│       └── public/               # (quando houver assets estáticos)
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
├── xtask/                        # cargo xtask: build-all, gen-ts (contratos → TS), schema, release, arch-guard (grafo de dependências: GUI/CLI/MCP só dependem de forge-ipc; GUI sem windows/windows-sys), lint-registry, registry-index, registry-sign, registry-keygen
├── scripts/                      # ci.ps1 (CI local), dev: setup.ps1, run-daemon.ps1, smoke.ps1; e2e-desktop.ps1 (e2e da janela real, M2-E1-T19); qa/ (capture-window.ps1: PrintWindow, window-probe.ps1)
├── tests/
│   ├── e2e/                      # testes de ponta a ponta contra daemon real (Windows runner)
│   └── fixtures/                 # zips mínimos fake de "tools" para testar providers/extract/layout;
│       │                         #   registry/ (assinado com a chave dev, 2 tools + 1 stack) e registry-bad-sha/ (sha errado no índice)
└── .github/
    └── workflows/                # ci.yml (fmt, clippy, build, test, deny, arch-guard, lint-registry, artefatos gerados, pnpm build), dependabot.yml, PULL_REQUEST_TEMPLATE.md, release.yml (build, sign, latest.json)
```

## Regras de organização

1. **Nada de lógica de tool específica fora de `registry/` e `forge-render/templates/`.** Se um agente sentir vontade de escrever `if tool.id == "mysql"` em um crate, a resposta é estender o schema do manifesto.
2. **Toda chamada Win32 fica em `forge-core/src/platform/windows/`** atrás de um trait. Outros crates só veem o trait.
3. **GUI e CLI nunca importam crates de domínio.** Só `forge-ipc`. `cargo xtask arch-guard` (parte de `xtask ci` e do CI) falha se `forge-cli`, `forge-mcp` ou `forgebox-desktop` dependerem de outro crate do workspace, ou se `forgebox-desktop` depender direto de `windows`/`windows-sys`. O xtask `gen-ts` gera os tipos TypeScript a partir dos contratos Rust (via `ts-rs` ou `specta`).
4. **Templates são dados**, embutidos no binário por `include_dir!`, mas o usuário pode sobrescrever qualquer um colocando o mesmo caminho em `<root>/etc/templates/` (ver [08-release-layout.md](08-release-layout.md)).
5. **Migrations SQLite são numeradas** (`0001_init.sql`...) e nunca editadas depois de publicadas.
6. **Cada crate tem `README.md` de 10 linhas** dizendo o que é, o que não é e quem depende dele.
7. **Testes:** unitários ao lado do código; integração em `crates/<x>/tests/`; e2e em `tests/e2e/` usando um `ForgeRoot` temporário e fixtures de tools fake (um `php.exe` fake que responde FastCGI, por exemplo).
