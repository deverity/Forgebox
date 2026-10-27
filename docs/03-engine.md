# 03 — Motor (engine)

O motor é o `forgeboxd`. Este documento define o comportamento interno que os crates `forge-supervisor`, `forge-fcgi`, `forge-render`, `forge-provider` e `forge-net` precisam implementar. É a especificação que faz o Forgebox ser confiável onde Laragon e Herd falham.

## 1. Supervisor de processos

### 1.1 Job Objects
- No boot, o daemon cria um Job Object raiz com `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK`.
- Cada `Instance` e cada `PhpPool` recebe um **Job Object filho** (aninhado, suportado desde Windows 8). Isso permite matar um serviço inteiro e seus netos (ex. `httpd.exe` pai + filhos) com uma chamada, sem varrer a árvore de processos.
- Processos que o usuário quer que sobrevivam ao daemon (raro; ex. um terminal aberto) são lançados fora do Job via `CREATE_BREAKAWAY_FROM_JOB`.

### 1.2 Spawn
```
SpawnSpec {
  exe: PathBuf,              # absoluto, dentro de bin/<tool>/<version>/
  args: Vec<String>,         # já renderizados (templates podem usar {{ paths.etc }} etc.)
  cwd: PathBuf,
  env: EnvMap,               # herda env mínimo + PATH montado pelo Forgebox + vars do manifesto
  stdio: Capture | Discard | File(path),
  console: Hidden,           # CREATE_NO_WINDOW; serviços não abrem janela de console
  job: JobHandle,
}
```
- `PATH` do processo filho é **determinístico**: `<root>/bin/<tool>/<version>` + dependências declaradas (`depends_on`) + `%SystemRoot%\System32`. Nunca herda o PATH do usuário (evita "funciona na minha máquina" por causa de um `php.exe` perdido em `C:\xampp`).
- stdout/stderr capturados em pipes assíncronos → `logs/<tool>/<instance>.out.log` com rotação (10 MB × 5) e broadcast em tempo real para assinantes IPC.

### 1.3 Parada graciosa (ordem de tentativa)
1. `stop.command` do manifesto (ex. `mysqladmin --port {{port}} shutdown`, `nginx -s quit`, `httpd -k stop`, `pg_ctl stop -m fast`). Timeout `stop.timeout` (padrão 15 s).
2. `CTRL_BREAK_EVENT` via `GenerateConsoleCtrlEvent` para o grupo do processo (requer `CREATE_NEW_PROCESS_GROUP` no spawn). Funciona para `php-cgi`, `node`, `mailpit`, `redis-server`.
3. `WM_CLOSE` para janelas top-level do PID (GUIs como HeidiSQL).
4. `TerminateJobObject`. Sempre funciona, nunca deixa órfão.

Cada etapa é um evento (`service.stopping{strategy}`), para que a UI mostre "Pedindo ao MySQL para desligar… (8 s)".

### 1.4 Máquina de estados da Instance
```
Stopped ──start──▶ Starting ──health ok──▶ Running ──stop──▶ Stopping ──▶ Stopped
   ▲                  │ health fail/exit                │ exit inesperado
   │                  ▼                                 ▼
   └──────────── Failed(reason) ◀────────────────── Crashed(code) ──restart policy──▶ Starting
                                                   
Qualquer estado ──install/upgrade em curso──▶ Maintenance
```
- `Starting` tem deadline (`health.start_timeout`, padrão 30 s). Health precisa passar `health.success_threshold` vezes.
- Políticas de restart: `never`, `on-failure{max: 3, window: 60s, backoff: 1s..30s}`, `always`.
- `Failed(reason)` carrega diagnóstico estruturado: porta ocupada (e por quem), DLL faltando (ex. VC++ Redist: detectado por exit code `0xC0000135` → UI oferece instalar), config inválida (saída do `-t`), permissão.

### 1.5 Pré-checagens antes de `Starting`
- **Validação de config**: se o manifesto declara `service.check` (`httpd -t`, `nginx -t`, `mysqld --validate-config`), roda antes. Falha = `Failed(ConfigInvalid)` com stderr anexado. Nunca derruba um serviço que estava funcionando por uma config quebrada.
- **Portas**: todas as portas declaradas são verificadas com `GetExtendedTcpTable`. Se ocupada por processo externo: `Failed(PortInUse{pid, exe})` com ação "Encerrar processo" (se for do usuário) ou "Trocar porta".
- **Dependências**: `depends_on` deve estar `Running`.
- **Runtime deps**: VC++ Redistributable (VS16/VS17/VS18) para PHP/Apache. Detecção por registro + oferta de download do `vc_redist.x64.exe` (manifesto `runtime: vcredist-vs17`).

## 2. Gerenciador FastCGI de PHP (`forge-fcgi`)

PHP para Windows não tem PHP-FPM. Laragon, Herd e WampServer todos improvisam. O Forgebox implementa um **gerenciador de pool de verdade**, e é isso que viabiliza "cada vhost com seu PHP".

### 2.1 Modelo
```
PhpPool {
  version: ToolVersion (php 8.3.36 nts-vs17-x64),
  ini_dir: etc/php/8.3/           # php.ini + conf.d/*.ini gerenciados
  workers: Vec<Worker>,           # cada um: php-cgi.exe -b 127.0.0.1:<porta>
  ports: faixa alocada,           # ex. 19083..19099 (ver §4)
  scaling: { min: 2, max: 12, idle_timeout: 120s },
  recycle: { max_requests: 500, max_rss_mb: 256, max_age: 1h },
  env: { PHP_FCGI_MAX_REQUESTS, PHPRC=ini_dir, PHP_INI_SCAN_DIR=ini_dir/conf.d, FORGEBOX_SITE=... },
}
```
- **Um processo por porta.** Não dependemos do comportamento indefinido de vários processos no mesmo socket (SO_REUSEADDR no Windows). O web server balanceia:
  - Nginx: `upstream php_8_3 { least_conn; server 127.0.0.1:19083; server 127.0.0.1:19084; ... }`
  - Apache: `<Proxy "balancer://php_8_3"> BalancerMember "fcgi://127.0.0.1:19083" ... </Proxy>` + `SetHandler "proxy:balancer://php_8_3"` (forma `SetHandler`, não `ProxyPass`; `ProxyFCGIBackendType GENERIC` mais um `ProxyFCGISetEnvIf` que remove a barra antes da letra do drive em `SCRIPT_FILENAME`, ver docs/06 §2.1). Sem reuso de conexão em nenhum servidor: `php-cgi` atende uma conexão por vez (`enablereuse=off max=1`; sem `fastcgi_keep_conn`/`keepalive`).
  - Caddy: `php_fastcgi 127.0.0.1:19083 127.0.0.1:19084 { lb_policy least_conn }`.
- **Scaling**: o pool observa conexões ativas (via `GetExtendedTcpTable` ESTABLISHED por porta) e `FCGI_GET_VALUES`; sobe workers até `max` sob carga e desce até `min` após `idle_timeout`. Ao mudar o número de workers, re-renderiza o bloco `upstream` e faz `reload` do web server (operação sem downtime em Nginx/Apache/Caddy).
- **Recycle**: `PHP_FCGI_MAX_REQUESTS` cuida do contador interno; o pool substitui o worker proativamente (sobe novo, remove do upstream, espera drenar, mata).
- **Warmup**: um worker novo só entra no upstream após responder a uma requisição FastCGI interna (`FCGI_GET_VALUES` + GET de `/_forgebox/health.php` renderizado pelo Forgebox).

### 2.2 Blue/green entre versões
Troca de `8.3.36 → 8.3.37` ou de um site de `8.2 → 8.4`:
1. Pool alvo é garantido ativo (sobe se não existir).
2. Vhost re-renderizado apontando para o novo upstream.
3. `reload` do web server.
4. Se nenhum site referencia o pool antigo → drenagem (espera conexões ESTABLISHED zerarem ou 30 s) → stop.

Zero requests perdidas, zero restart de Apache/Nginx.

### 2.3 Xdebug e extensões
- Extensões são `kind: Extension` no registry, com `extends: php` e `compat: [{ tool: "~8.4", variant: { ts: nts, compiler: vs17 }, version: "3.4.*" }]` ([04](04-tool-manifest-spec.md) §2). Instaladas em `bin/php/8.3.36/ext/` + `etc/php/8.3/conf.d/20-xdebug.ini`.
- Toggle de Xdebug é por **versão** (afeta o pool) ou por **site** (via `FORGEBOX_XDEBUG=1` → `php.ini` com `xdebug.mode=${XDEBUG_MODE}`). Reload = recycle do pool, não restart.
- Porta do Xdebug (9003) é reservada no alocador para nunca colidir com pools (bug histórico do Laragon #374).

### 2.4 FrankenPHP (alternativa)
Sites com `runtime: frankenphp` não usam pool: o Forgebox sobe um `frankenphp.exe` por site (worker mode opcional) como `App` e o web server frontal faz proxy, ou o FrankenPHP serve direto (Caddy embutido) nas portas 80/443 se for o único servidor. Útil para Laravel Octane.

## 3. Renderização de configuração (`forge-render`)

### 3.1 Princípios
- **Fonte da verdade é o banco + manifestos; arquivos em `etc/` são derivados.** Mas o usuário pode editar: tudo que está fora dos blocos gerenciados é preservado.
- Blocos gerenciados:
  ```
  # forgebox:begin sites (hash=ab12…)  ← não edite dentro deste bloco
  ...
  # forgebox:end sites
  ```
  Dentro: o Forgebox reescreve. Fora: o Forgebox nunca toca. Se o usuário editar dentro, `drift` é detectado e a UI pergunta "manter sua edição (congela bloco) ou regenerar".
- Arquivos 100% gerados (vhosts individuais, `upstreams.conf`, `conf.d/*.ini`) têm cabeçalho `# generated by forgebox — edits will be lost; override via etc/templates/`.
- Overrides de template: qualquer arquivo em `<root>/etc/templates/<mesmo caminho>` substitui o embutido. Útil para empresas com padrões próprios.

### 3.2 Alvos por tool (exemplos)
| Tool | Arquivos renderizados | Reload |
|---|---|---|
| Apache | `etc/apache/httpd.conf` (include fixo; a versão exata entra por `tool.dir`), `etc/apache/conf.d/*.conf`, `etc/apache/sites/<host>.conf`, `etc/apache/upstreams/php_<ver>.conf` | `httpd -k graceful` |
| Nginx | `etc/nginx/nginx.conf`, `etc/nginx/conf.d/*.conf`, `etc/nginx/sites/<host>.conf`, `etc/nginx/upstreams/php_<ver>.conf` | `nginx -s reload` |
| Caddy | `etc/caddy/Caddyfile` (import sites/*) | `caddy reload` |
| PHP | `etc/php/<major.minor>/php.ini`, `conf.d/10-forgebox.ini`, `conf.d/20-xdebug.ini` | recycle do pool |
| MySQL/MariaDB | `etc/mysql/<instance>/my.ini` | restart |
| PostgreSQL | `etc/postgresql/<instance>/postgresql.conf`, `pg_hba.conf`, `pg_ident.conf` | `pg_ctl reload` |
| Mailpit | argumentos de linha de comando | restart |

### 3.2.1 Implementação (M1-E4-T1)
- `crates/forge-render/templates/<tool>/…` (embutidos com `include_dir!`) + `targets/<tool>.rs`: cada alvo é uma tabela estática de `FileSpec` (template, caminho em `etc/`, escopo global/site/site-ssl/pool, bloco gerenciado ou arquivo gerado). Hoje é constante Rust; o destino final é o manifesto declarar isso em `config.files`.
- Layout: `etc/nginx/nginx.conf` e `etc/apache/httpd.conf` (sem pasta de versão; a versão exata entra por `tool.dir`), `etc/php/<série>/conf.d/10-forgebox.ini` (overlay; o `php.ini` do usuário nunca é tocado), `etc/mysql/<instância>/my.ini`, `etc/postgresql/<instância>/{postgresql.conf,pg_hba.conf}`, `etc/mailpit/<instância>.args` (um argumento por linha).
- Blocos gerenciados (`main`) em `nginx.conf`, `httpd.conf`, `Caddyfile`, `my.ini`, `postgresql.conf`; vhosts, snippets, upstreams, `conf.d/*.ini`, `pg_hba.conf` e `fastcgi_params` são 100 % gerados (cabeçalho `# generated by forgebox …`; `;` em INI). Hash do bloco = 16 primeiros hex do SHA-256 do corpo normalizado em LF.
- Os diretórios `etc/<servidor>/{conf.d,snippets,upstreams,sites}` são criados por `Target::ensure_dirs` (Apache falha em `IncludeOptional dir/*.conf` se `dir` não existir). `logs/<servidor>` e `tmp/<servidor>` (logs, pid, temporários) precisam existir antes do `-t` e do start: responsabilidade do supervisor (M1-E3).
- **Nginx no Windows rejeita caminhos com `~` (nomes 8.3, ex. `GUSTAV~1`)**: a raiz deve ser resolvida para o nome longo antes de renderizar.
- `PhpModel.zend_extensions: Vec<String>` (preenchido pelo daemon via `ExtensionsConfig::zend_extensions_for(scheme, &versão)`, que aplica o `constraint` opcional de cada item de `config.extensions.zend_extensions`) lista as linhas `zend_extension = …` do overlay. `opcache` só entra quando é uma DLL separada; o PHP 8.5+ embute o OPcache e carregar `opcache` emite `Warning: Failed loading Zend extension` a cada inicialização. As diretivas `opcache.*` continuam sendo emitidas.
- Diretórios de runtime do PHP usados pelo overlay (`session.save_path`, `upload_tmp_dir`): `tmp/php/<série>/sessions` e `tmp/php/<série>/uploads`, ambos criados pelo `post_install` do manifesto (docs/04).
- `StaticLocation.cache_control` substitui o `expires` do rascunho (uniforme nos três servidores).

### 3.3 Contexto disponível nos templates
`paths.*` (root, bin, etc, data, logs, tmp, www, certs), `tool`, `version`, `instance` (name, ports, data_dir, settings), `sites[]` (cada um um `VhostModel`: `kind` php/static/proxy, `hosts`, `root`, `index`, `php_upstream`, `try_files`, `static_locations`, `deny`, `headers`, `client_max_body`, `ssl`, `http_redirect`, `proxy`, `env`, `extra`, `fallback_resource` derivado), `pools[]` (`UpstreamModel`), `php` (`PhpModel`: série, `zend_extensions`, extensões, xdebug), `prefs` (portas http/https/internal, document_root, tld...). Nos templates por site, `site` é o `VhostModel` atual e `upstream` o pool atual. Valores de `env`/`headers` não podem conter `$` nem `%{` (interpolados por nginx/Apache): `VhostModel::normalized` os rejeita com `invalid_input`. Filtros: `win_path`, `posix_path`, `quote`, `indent`, `to_yaml`.

## 4. Portas

- Faixas reservadas (configuráveis em `forgebox.toml`):
  - Web: 80/443 (Apache **ou** Nginx **ou** Caddy frontal; outros em 8080/8443 por padrão quando dois rodam juntos).
  - PHP pools: 19000–19999. Xdebug: 9003 (reservada).
  - Bancos: MySQL 3306+, MariaDB 3307+ (sugerido quando ambos), PostgreSQL 5432+, Mongo 27017+, Redis 6379+, Memcached 11211.
  - Mail: SMTP 1025, UI 8025. Storage: MinIO 9000/9001. Apps: 3000–3999.
- Em `forgebox.toml` as faixas são nomeadas e referenciadas pelos manifestos (`port_range: php-pools`, `ports[].range: databases`): `[ports.ranges]` com `web = [80, 443]`, `web-alt = [8080, 8443]`, `php-pools = ["19000-19999"]`, `xdebug = [9003]`, `databases = ["3306-3399", "5432-5499", "6379-6399", "11211-11299", "27017-27099"]`, `apps = ["3000-3999"]`, `mail = [1025, 8025]`, `storage = [9000, 9001]`; `[ports] reserved = [9003]` (nunca alocada automaticamente). Faixas ausentes no arquivo mantêm o padrão (`forge_core::config`).
- Alocação persistida em `ports` (SQLite). Uma porta nunca é "adivinhada" duas vezes.
- Conflito externo: `GetExtendedTcpTable` → PID → nome do exe → UI mostra "Porta 3306 em uso por `mysqld.exe` (C:\xampp\mysql\bin) — PID 4412". Ações: encerrar (se do usuário), mudar porta (re-render automático), ignorar.

## 5. Providers, instalação e layout (`forge-provider`)

### 5.1 Pipeline de instalação
```
resolve(version) → fetch(url, sha256, size) → verify → stage(tmp/stage/<id>/) → extract → post_install hooks
→ move atômico para bin/<tool>/<version>/ → register em SQLite → (opcional) current junction → render config inicial
```
- Download com retomada (`Range`), 4 conexões paralelas para arquivos > 50 MB, mirrors em ordem, limite de banda configurável.
- Verificação **obrigatória**: SHA-256 do manifesto/provider. Sem hash = instalação só com `--allow-unverified` e aviso vermelho na UI.
- Staging fora do destino + `MoveFileEx` garante que `bin/<tool>/<version>` nunca fica meio instalado. Implementação (`forge_provider::extract`, [ADR-0017](adr/0017-verify-extract-stack.md)): `tmp/stage/<ulid>/` novo por extração, com a sensibilidade a maiúsculas do pai do destino (a flag viaja com o diretório no rename; `bin/` fica insensitive, docs/15); cada entrada validada antes de ser escrita (zip e 7z varridos por inteiro antes do primeiro byte); orçamento de bytes realmente escritos (padrão 4 GiB) e de entradas (200 000); `strip_root`; rename do diretório (repetido por até ~1,4 s em violação de acesso/compartilhamento, comum com antivírus); entre volumes, cópia para `.<versão>.forgebox-partial-<id>` ao lado do destino, conferência de caminhos e tamanhos e rename. Substituir um destino existente só com pedido explícito (o antigo vai para `.<versão>.forgebox-old-<id>` e volta se a troca falhar). Progresso por entrada em `job.progress` ("Extraindo 120/2400"); cancelamento entre entradas e buffers; staging sempre removido.
- `post_install` restrito a ações declarativas do manifesto (copiar arquivo, renderizar template, rodar comando **da própria tool** como `mysqld --initialize-insecure`), nunca scripts arbitrários baixados.

### 5.2 Layout versionado + `current`
```
bin/php/8.3.36/        bin/php/8.4.14/       bin/php/current → 8.4.14 (junction)
bin/node/20.19.0/      bin/node/22.12.0/     bin/node/current → 22.12.0
bin/apache/2.4.69/     bin/nginx/1.30.6/
```
- `current` é **junction** (não symlink): não exige privilégio nem Developer Mode. Troca atômica via `current.tmp` + rename.
- No onboarding, `<root>/bin/<tool>/current` das tools de CLI (php, composer, node, python, git...) é adicionado **uma vez** ao PATH do usuário (HKCU, com broadcast `WM_SETTINGCHANGE`). Depois disso, trocar versão nunca mais toca em env-vars. Também geramos `bin/shims/*.cmd` como alternativa para quem não quer alterar PATH.
- Pools web não dependem de `current`: cada site aponta para a versão exata.

### 5.3 Descoberta de tools soltas
`forge-registry::discover` varre `bin/*/*/` e reconhece pastas sem registro (ex. o usuário copiou `php-8.1-Win32-vs16-x64` do Laragon). Heurísticas por manifesto (`discover.match_files: ["php.exe", "php-cgi.exe"]`, `discover.version_command: "php.exe -v"`, `version_regex`, `variant_detect`; [04](04-tool-manifest-spec.md) §1). Reconhecidas → importadas como `ToolVersion{source: manual}`; podem virar pools normalmente.

## 6. Scheduler interno
- Tarefas periódicas (tokio): checagem de updates (padrão 6 h, jitter), renovação de certificados (diária), limpeza de `tmp/` (diária), refresh do registry (12 h), métricas (2 s), health (5 s por instance).
- Todas respeitam `prefs.metered_connection` e `prefs.quiet_hours`.

## 7. Modelo de concorrência: nada trava o controlador, nenhum serviço trava outro

Regra de ouro: **o daemon nunca espera por uma tool.** Toda operação lenta ou potencialmente travável roda isolada, com timeout, e reporta progresso por eventos. Detalhes normativos em [ADR-0011](adr/0011-actor-concurrency-model.md).

### 7.1 Runtime
- `tokio` multi-thread: `worker_threads = max(4, núcleos)`; pool de bloqueio (`spawn_blocking`) dimensionado para `2 × núcleos`, usado para **tudo** que é síncrono: I/O de arquivo grande, extração de zip/7z, hashing, chamadas Win32 que podem demorar (`GetExtendedTcpTable`, `WinVerifyTrust`, APIs de certificado), `CreateProcess`.
- SQLite em **uma thread dedicada de escrita** (fila mpsc; transações curtas; WAL) + conexões de leitura em pool. Nenhuma query roda numa worker thread do tokio.
- Em dev, `tokio-console` habilitado (`--cfg tokio_unstable`) para detectar tasks que bloqueiam > 1 ms.
- Implementação ([ADR-0014](adr/0014-concurrency-skeleton.md)):
  - `forge_core::runtime::build_runtime(&RuntimeConfig::default())`: tamanhos acima, threads nomeadas `forgebox-rt-N`. `tokio-console` é a feature opt-in `console` de `forge-core` (`runtime::init_console()`), compilada com `RUSTFLAGS="--cfg tokio_unstable"`; o build padrão não usa `tokio_unstable`.
  - `Store::writer()` → `StoreWriter::execute(|conn| …)` (timeout padrão 5 s; `execute_timeout` para outro valor). A thread `forgebox-db-writer` agrupa o que já está na fila em transações de até 64 operações ou 5 ms, sem esperar por mais trabalho; cada operação roda num `SAVEPOINT` (erro ou pânico desfaz só ela) e as respostas saem depois do `COMMIT`. Operação cujo chamador desistiu antes de começar é pulada; `Timeout` de uma operação já iniciada significa "resultado desconhecido" (escritas idempotentes).
  - `Store::reader()` → `StoreReader::query(|conn| …)`: `spawn_blocking` com timeout sobre um pool de conexões `query_only`; com WAL a leitura nunca espera o writer.
  - `Store::conn()` é a conexão de manutenção (migrações no boot, ferramentas offline, testes), nunca usada em código async.

### 7.2 Atores
Cada `Instance`, cada `PhpPool`, cada `App` (process), o DNS server, o servidor IPC, o scheduler, o download manager e o metrics collector são **atores**: uma task própria com mailbox (`mpsc`) e estado privado. Consequências:
- Um `mysqladmin shutdown` que trava afeta só o ator do MySQL, que tem timeout e escala para a próxima estratégia de parada. Nginx, PHP, IPC e GUI seguem respondendo.
- Comandos entre atores são mensagens com `oneshot` de resposta e **sempre** `tokio::time::timeout`. Nunca há chamada direta de método entre atores.
- Sem `std::sync::Mutex`/`RwLock` segurado através de `.await`. Estado compartilhado só por mensagem; `parking_lot::Mutex` apenas para seções críticas de nanosegundos (contadores, caches) e nunca aninhado fora da ordem documentada.
- Ordem de travas, quando inevitável, é documentada no módulo e verificada por `debug_assert` (hoje um único aninhamento: `jobs.table` → `event_bus.seq`, para publicar `job.*` em ordem).
- Um ator que entra em pânico é isolado (`catch_unwind` na fronteira da task) e reiniciado pelo supervisor de atores; o evento `actor.restarted` é emitido. O daemon **nunca** morre por culpa de uma tool.
- Implementação (`forge_core::actor`, [ADR-0014](adr/0014-concurrency-skeleton.md)):
  - `trait Actor { type Msg; handle; on_start; on_stop; on_restart }`; `ActorRef::tell(msg)` nunca espera (fila cheia → `MailboxFull`); `ActorRef::ask(|reply| Msg::X(reply), timeout)` sempre com timeout → `Timeout | Stopped | MailboxFull`.
  - `ActorSupervisor::spawn(ActorSpec, factory)`: o loop de supervisão roda no task do próprio ator e reaproveita a mesma mailbox (os `ActorRef` sobrevivem ao reinício). `RestartPolicy` padrão: 5 reinícios em 60 s, backoff 100 ms dobrando até 5 s; esgotada → status `Failed`, evento `actor.failed`, `ask` → `Stopped`.
  - `ActorHandle::stop()`: fecha a mailbox, drena o que estava nela e roda `on_stop` dentro de `stop_timeout` (5 s); depois aborta o task e devolve `Aborted` (quem pede a parada nunca fica preso).
  - Travas: `forge_core::sync::OrderedMutex` com `LockLevel`; tabela de ordem no topo de `actor/mod.rs`; `clippy::await_holding_lock`, `await_holding_refcell_ref` e `await_holding_invalid_type` (guard do `OrderedMutex`) são erro.

### 7.3 Pipes de processo
- stdout/stderr de cada processo têm tasks bombeadoras próprias. Escrita em arquivo é garantida (fila limitada com backpressure para o processo, como um terminal faria); broadcast para assinantes da UI é *drop-oldest* (um cliente lento nunca segura o daemon).
- Health probes, sampler de métricas e watchers de arquivos rodam em tasks de intervalo com jitter; chamadas Win32 dentro deles vão para `spawn_blocking`.

### 7.4 API assíncrona por natureza
- Nenhum método JSON-RPC segura a conexão mais de ~1 s, exceto streams. Operações longas (instalar, iniciar serviço, trocar PHP, exportar banco) devolvem **imediatamente** um `job_id`; progresso e conclusão vêm por eventos (`job.progress`, `job.done`, `job.failed`). CLI mostra barra; GUI mostra estado otimista.
- Implementação (`forge_core::jobs`): `JobRegistry::start(nome, |ctx| async { … })` devolve o `JobId` na hora; `ctx.progress(JobProgress::new().pct(40.0).message("…"))` (também `.bytes(done, total)` e `.rate(speed_bps, eta_secs)`) (no máximo um `job.progress` a cada 100 ms; o snapshot de `get/list` é sempre atual); `job.cancel` → `CancellationToken` cooperativo (`ctx.cancelled()`, `ctx.check_cancelled()?`), e após 10 s de tolerância o future é descartado (`job.cancelled{forced: true}`); pânico → `job.failed` com código `internal`; `request_id` repetido devolve o mesmo job; ficam retidos os últimos 100 jobs terminados. Eventos de um job saem em ordem de `seq` (`started < progress < done|failed|cancelled`).
- Prioridade: ações do usuário sobre tarefas de fundo. O download manager tem fila com prioridade; o scheduler de updates pausa quando há job do usuário em curso; o sampler de métricas reduz frequência sob carga.
- O servidor IPC aceita conexões em task própria; cada conexão tem sua task; parsing de JSON não acontece na task de accept.

### 7.5 GUI e CLI
- Todos os comandos Tauri são `async` e finos (encaminham ao daemon). A thread de UI do WebView nunca espera I/O. Eventos são aplicados em lote por frame (`requestAnimationFrame`), stores por serviço evitam re-render global.
- A GUI continua navegável com o daemon ocupado ou até parado (mostra estado "reconectando" e enfileira ações que fazem sentido).
- A CLI usa streams para `logs -f`, `stats --watch`, `status --watch`; `Ctrl+C` cancela o job no daemon (`job.cancel`).

### 7.6 Orçamentos e testes
- Nenhuma seção bloqueante em contexto async > 1 ms (verificado por `tokio-console` em CI de desempenho).
- Teste de "serviço travado": processo fake que ignora CTRL-C e nunca encerra → o resto do daemon deve responder a `forge status` em < 50 ms durante todo o stop em cascata.
- Teste de "cliente lento": assinante de logs que não lê → nenhum impacto em throughput de outros clientes nem no processo monitorado.
- Teste de "download pesado + start de serviço": start não pode atrasar mais que 10 % em relação ao baseline.

## 8. Eventos (nomes canônicos)
`daemon.ready`, `daemon.shutdown`, `job.started|progress|done|failed|cancelled`, `actor.restarted`, `actor.failed`, `fs.case_sensitivity_applied|unavailable`, `tool.install.started|progress|completed|failed`, `tool.updated`, `tool.rollback`, `service.starting|running|stopping|stopped|crashed|failed`, `pool.scaled`, `pool.worker.recycled`, `site.created|updated|removed`, `site.php_changed`, `config.rendered`, `config.drift_detected`, `dns.policy_applied`, `tls.ca_created|leaf_issued|renewed`, `port.conflict`, `helper.op{name, ok}`.

Cada evento é `{id: ULID, seq, ts (RFC 3339 UTC), kind, payload}` (`forge_core::event`). `seq` cresce de 1 em 1 e continua após restart (o daemon semeia o `EventBus` com o maior `seq` da tabela `events`), o que permite `resume_from(seq)`. O bus é `tokio::sync::broadcast` (4096 por assinante): publicar nunca bloqueia; um assinante lento perde os mais antigos (drop-oldest) e recebe a contagem de perdidos para ressincronizar pelo SQLite.

Payloads são structs serde versionadas (campo `v`). As de `job.*` (`forge_core::jobs::payload`) e `actor.*` (`forge_core::actor::{ActorRestartedPayload, ActorFailedPayload}`) ficam em `forge-core`, junto de quem as publica, e serão re-exportadas por `forge-ipc` nos contratos (M1-E5-T1); as demais nascem em `forge-ipc`. A GUI é 100% reativa a esses eventos; nenhuma tela faz polling além de métricas.
