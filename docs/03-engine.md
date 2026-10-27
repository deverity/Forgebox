# 03 — Motor (engine)

O motor é o `forgeboxd`. Este documento define o comportamento interno que os crates `forge-supervisor`, `forge-fcgi`, `forge-render`, `forge-provider` e `forge-net` precisam implementar. É a especificação que faz o Forgebox ser confiável onde Laragon e Herd falham.

## 1. Supervisor de processos

### 1.1 Job Objects
- No boot, o daemon cria um Job Object raiz com `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK`.
- Cada `Instance` e cada `PhpPool` recebe um **Job Object filho** (aninhado, suportado desde Windows 8). Isso permite matar um serviço inteiro e seus netos (ex. `httpd.exe` pai + filhos) com uma chamada, sem varrer a árvore de processos.
- Processos que o usuário quer que sobrevivam ao daemon (raro; ex. um terminal aberto) são lançados fora do Job via `CREATE_BREAKAWAY_FROM_JOB`.
- Implementação (`forge_core::platform::{ProcessControl, ProcessGroup}`, impl em `platform/windows/process.rs`; `forge_supervisor::JobTree`):
  - Job de instância: `KILL_ON_JOB_CLOSE | BREAKAWAY_OK | DIE_ON_UNHANDLED_EXCEPTION` (um serviço que quebra sai com o código da exceção em vez de ficar preso no diálogo do WER).
  - O aninhamento nasce na primeira atribuição: todo processo é atribuído da raiz para baixo (`AssignProcessToJobObject` raiz → … → job da instância). Funciona igual com o daemon dentro de um Job externo (cargo, terminais e IDEs fazem isso): a raiz vira filha desse Job.
  - `QueryInformationJobObject(JobObjectBasicProcessIdList)` lista os PIDs (inclui jobs aninhados). O `conhost.exe` do console oculto do serviço também é membro do Job.
  - **Terminar um Job é assíncrono.** `TerminateJobObject` retorna antes de os processos saírem, e o Job recusa `AssignProcessToJobObject` com `ERROR_ACCESS_DENIED` por alguns milissegundos **mesmo depois de vazio** (medido no Windows 10 19045: `ActiveProcesses` chega a 0 em ~100 µs; as recusas duram 1–8 ms ocioso e dezenas de ms sob carga; nada observável — estado sinalizado do Job, contabilidade, `SetInformationJobObject` — indica quando acabam). Por isso:
    - `ProcessGroup::terminate_all` termina **e espera** o Job esvaziar (`JobObjectBasicAccountingInformation.ActiveProcesses == 0`, até `TERMINATE_TIMEOUT` = 5 s; `Timeout` se algum processo sobreviver). Nenhuma execução é dada como encerrada (`service.crashed`, `service.stopped`) com processos do Job ainda vivos: a limpeza de sobras (§1.2), a etapa 4 da cascata (§1.3) e o encerramento do ator usam essa espera.
    - A atribuição de um processo novo (`spawn`/`assign`) repete `ERROR_ACCESS_DENIED` com backoff (250 µs → 20 ms) por até `ASSIGN_RETRY_LIMIT` = 2 s; se ainda falhar, o filho suspenso é terminado e o erro diz que o Job pode estar terminando.
    - Terminações disparadas fora de uma execução (processo de uma execução que o ator não acompanha mais) são aguardadas pela próxima execução antes do spawn. Um `stop` que falhou (o processo não saiu após `TerminateJobObject`) não emenda o `restart` pendente: ele falha com o mesmo erro.
    - Alternativa rejeitada: um Job novo por execução. Dispensaria a repetição no spawn, mas espalharia o estado da instância por vários Jobs (cascata, `WM_CLOSE`, listagem de PIDs e sobras de execuções anteriores teriam de acompanhar Jobs antigos) e ainda exigiria a espera para não dar uma execução por encerrada com processos vivos.

### 1.2 Spawn
```
SpawnSpec {                  # forge_supervisor::SpawnSpec
  exe: PathBuf,              # absoluto, dentro de bin/<tool>/<version>/ (.bat/.cmd recusados)
  args: Vec<String>,         # já renderizados (templates podem usar {{ paths.etc }} etc.)
  cwd: PathBuf,
  env: EnvMap,               # vars do manifesto (nomes case-insensitive); PATH proibido aqui
  path: Vec<PathBuf>,        # <root>/bin/<tool>/<version> + pastas de depends_on, nesta ordem
  stdio: Capture | Discard | File(path),
  console: Hidden,           # CREATE_NO_WINDOW; serviços não abrem janela de console
  new_process_group: bool,   # CREATE_NEW_PROCESS_GROUP (manifesto console_group, padrão true)
}
```
- O Job não faz parte do spec: o ator da instância cria o processo no seu próprio Job (§1.1).
- `PATH` do processo filho é **determinístico**: `SpawnSpec::path` + `%SystemRoot%\System32` (`GetSystemDirectoryW`). Nunca herda o PATH do usuário (evita "funciona na minha máquina" por causa de um `php.exe` perdido em `C:\xampp`).
- Ambiente = allowlist mínima herdada do daemon (`ProcessControl::base_env`: `SystemRoot`, `windir`, `SystemDrive`, `ComSpec`, `PATHEXT`, `TEMP`/`TMP`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles*`, `CommonProgramFiles*`, `PUBLIC`, `ALLUSERSPROFILE`, `HOMEDRIVE`/`HOMEPATH`, `USERNAME`/`USERDOMAIN`/`COMPUTERNAME`, `OS`, `NUMBER_OF_PROCESSORS`, `PROCESSOR_*`) + vars do manifesto + `PATH`. Nada mais vaza do ambiente do daemon.
- `CreateProcessW(CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT)` → atribuição aos Jobs → `ResumeThread`: nenhum filho roda (nem cria netos) fora do Job. Se a atribuição falhar, o processo é terminado ainda suspenso.
- stdin é sempre `NUL`. A herança de handles é restrita aos três handles de stdio (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`): spawns concorrentes nunca herdam os pipes uns dos outros (o que atrasaria o EOF).
- stdout/stderr capturados em named pipes (nosso lado overlapped no reator do tokio, sem thread por pipe; lado do filho síncrono) → `logs/<tool>/<instance>.out.log` com rotação (10 MiB × 5 arquivos no total: `.out.log`, `.out.log.1` … `.out.log.4`) e broadcast em tempo real para assinantes IPC (§7.3). `File(path)`: o próprio filho anexa ao arquivo (sem rotação, sem visão ao vivo).
- A saída do processo é observada por `RegisterWaitForSingleObject` (pool de threads do SO), sem thread bloqueada por processo. Quando o processo principal sai, o que restar no Job da instância (netos, e o `conhost.exe` que eles mantêm vivo) ganha 300 ms para sair sozinho e então é terminado, esperando o Job esvaziar (§1.1); a contagem vai em `leftovers_killed` de `service.stopped`/`service.crashed`.

### 1.3 Parada graciosa (ordem de tentativa)
1. `stop.command` do manifesto (ex. `mysqladmin --port {{port}} shutdown`, `nginx -s quit`, `httpd -k stop`, `pg_ctl stop -m fast`). Roda num Job próprio, stdio descartado, mesmo `PATH`/ambiente do serviço. Saída ≠ 0 escala na hora; saída 0 continua esperando o serviço sair.
2. `CTRL_BREAK_EVENT` via `GenerateConsoleCtrlEvent` para o grupo do processo (requer `CREATE_NEW_PROCESS_GROUP` no spawn). Funciona para `php-cgi`, `node`, `mailpit`, `redis-server`. Entregue pelo auxiliar `forgebox-ctrl.exe` (ver abaixo); pulada se o serviço não tem grupo próprio.
3. `WM_CLOSE` para janelas top-level dos processos do Job da instância (GUIs como HeidiSQL). Só roda (e só espera) se houver janela.
4. `TerminateJobObject`, esperando o Job esvaziar (§1.1). Sempre funciona, nunca deixa órfão; um `restart` cria o processo novo no mesmo Job logo em seguida.

Esperas: a **primeira** etapa graciosa que roda espera `stop.timeout` (padrão 15 s); cada etapa graciosa seguinte espera `followup_grace` (5 s). Um serviço sem `stop.command` que ignora `CTRL_BREAK` (e não tem janela) é terminado `stop.timeout` depois do pedido. `StopPlan::worst_case()` dá o limite da cascata inteira (inclui os 5 s de `TERMINATE_TIMEOUT` para o Job esvaziar).

Cada etapa que roda é um evento (`service.stopping{strategy, step, timeout_ms}`, publicado antes de agir), para que a UI mostre "Pedindo ao MySQL para desligar… (8 s)"; ao fim, `service.stopped{strategy, exit_code, elapsed_ms, leftovers_killed}`. A cascata roda em task própria: o ator da instância continua respondendo `Status` durante uma parada travada (§7.6).

**`CTRL_BREAK` e consoles ([ADR-0015](adr/0015-ctrl-break-helper-process.md)).** `GenerateConsoleCtrlEvent` só alcança processos no console de quem chama, e anexar-se a outro console é estado do processo inteiro. O daemon nunca troca de console: executa `forgebox-ctrl.exe break <pid>` (sem console próprio, `DETACHED_PROCESS`), que se anexa ao console oculto do serviço, gera o evento para o grupo e sai (resultado no código de saída). Limitações: o serviço precisa de console (`console: Hidden`, padrão) e grupo próprio; o evento atinge o grupo inteiro naquele console (como Ctrl+Break num terminal); custa um processo de vida curta (dezenas de ms) por parada; se o auxiliar faltar, a etapa falha com log e a cascata segue.

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
- Implementação (`forge_supervisor::InstanceActor`, um ator por Instance): M1-E3-T1 entrega `Stopped`, `Starting`, `Running` (assim que o processo foi criado e retomado; health em M1-E3-T2), `Stopping{strategy}`, `Crashed{exit_code}` e `Failed{code, reason}` (`code` = `ForgeError::code`). Mensagens: `Start`, `Stop`, `Restart`, `Status`, `SubscribeLogs`; `Start`/`Stop`/`Restart` respondem quando a operação termina (pid ou `StopReport`), mas o ator nunca espera: spawn, observação da saída e cascata de parada rodam em tasks e voltam por mensagens internas. Políticas de restart e `Maintenance` ficam para M1-E3-T2.

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
  - Apache: `<Proxy "balancer://php_8_3"> BalancerMember "fcgi://127.0.0.1:19083" ... </Proxy>` + `SetHandler "proxy:balancer://php_8_3"` (forma `SetHandler`, não `ProxyPass`, para `SCRIPT_FILENAME` correto; `ProxyFCGIBackendType GENERIC`).
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
| Apache | `etc/apache/<ver>/httpd.conf` (include fixo), `etc/apache/conf.d/*.conf`, `etc/apache/sites/<host>.conf`, `etc/apache/upstreams/php_<ver>.conf` | `httpd -k graceful` |
| Nginx | `etc/nginx/<ver>/nginx.conf`, `etc/nginx/conf.d/*.conf`, `etc/nginx/sites/<host>.conf`, `etc/nginx/upstreams/php_<ver>.conf` | `nginx -s reload` |
| Caddy | `etc/caddy/Caddyfile` (import sites/*) | `caddy reload` |
| PHP | `etc/php/<major.minor>/php.ini`, `conf.d/10-forgebox.ini`, `conf.d/20-xdebug.ini` | recycle do pool |
| MySQL/MariaDB | `etc/mysql/<instance>/my.ini` | restart |
| PostgreSQL | `etc/postgresql/<instance>/postgresql.conf`, `pg_hba.conf` | `pg_ctl reload` |
| Mailpit | argumentos de linha de comando | restart |

### 3.3 Contexto disponível nos templates
`paths.*` (root, bin, etc, data, logs, tmp, www, certs), `tool`, `version`, `instance` (name, ports, data_dir, settings), `sites[]` (host, root, php_pool, ssl, aliases, env), `pools[]`, `prefs` (document_root, tld...). Filtros: `win_path`, `posix_path`, `quote`, `indent`, `to_yaml`.

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
- Implementação (`forge_supervisor::logs`): bomba por pipe (leitura overlapped, quebra de linha em `\n`, `\r` final removido, linhas > 16 KiB cortadas) → hub único por execução (numera `seq` por instância, contínuo entre reinícios; carimba `ts`) → `tokio::sync::broadcast` (1024 linhas por assinante; `LogItem::Lagged{missed}` para quem atrasou) e fila limitada (1024 linhas) → escritor de arquivo em lotes via `spawn_blocking` com rotação. Arquivo lento enche a fila, o hub espera, as bombas param de ler, o buffer de 64 KiB do pipe enche e o processo bloqueia no `WriteFile`; o broadcast nunca espera.
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

Payloads são structs serde versionadas (campo `v`). As de `job.*` (`forge_core::jobs::payload`) e `actor.*` (`forge_core::actor::{ActorRestartedPayload, ActorFailedPayload}`) ficam em `forge-core`, e as de `service.*` em `forge_supervisor::events` (`ServiceStartingPayload`, `…RunningPayload`, `…StoppingPayload`, `…StoppedPayload`, `…CrashedPayload`, `…FailedPayload`; todas começam com `instance_id`, `tool`, `instance`), junto de quem as publica, e serão re-exportadas por `forge-ipc` nos contratos (M1-E5-T1); as demais nascem em `forge-ipc`. A GUI é 100% reativa a esses eventos; nenhuma tela faz polling além de métricas.
