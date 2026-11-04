# 09 — CLI `forge`

`forge` é um cliente fino da API do daemon (`forge-ipc`). Se o daemon não está rodando, `forge` o inicia (exceto `--no-daemon`). Toda saída tem forma humana (tabelas, cores, spinners) e `--json` (estável, versionada). Códigos de saída: 0 ok · 1 erro genérico · 2 uso inválido · 3 recurso não encontrado · 4 conflito (porta/host) · 5 precisa de elevação · 6 verificação falhou.

```
forge <grupo> <comando> [args] [--json] [--quiet] [--yes] [--root <dir>] [--context <perfil>] [--no-daemon] [--no-elevate]
```

Flags globais (valem em qualquer posição, antes ou depois do comando): `--json` saída JSON · `--quiet`/`-q` só o resultado · `--yes`/`-y` confirma sem perguntar · `--root` raiz da instalação (padrão: `FORGEBOX_ROOT`, `portable.flag`, `root.toml`, `C:\Forgebox`) · `--context` perfil de conexão · `--no-daemon` não inicia o daemon · `--no-elevate` não dispara o UAC. Variáveis: `NO_COLOR` (sem cores), `CLICOLOR_FORCE` (cores em pipe), `FORGE_LANG=en` (mensagens em en-US; o padrão é pt-BR), `FORGE_CONFIG_DIR` (pasta dos arquivos do cliente), `FORGE_EDITOR`/`VISUAL`/`EDITOR` (editor de `service config` e `php ini --open`), `FORGE_DAEMON_EXE` (caminho do `forgeboxd`).

## Grupos e comandos

### daemon
```
forge daemon status | start | stop | restart | logs [-f] [-n N] | doctor
forge doctor [--export <arquivo>]   # diagnóstico completo: PATH, DNS, CA, portas, vcredist, Defender, versões
```
`daemon status|start|stop|restart` nunca iniciam o daemon só para perguntar (ou parar) algo: `status` de um daemon parado diz "parado" e sai com 1 (`{"running": false}` no `--json`); `stop` de um daemon parado sai com 0; `restart` de um parado o inicia. `stop` e `restart` só voltam quando o pipe sumiu / o novo daemon responde. `doctor` roda o job `daemon.doctor` (checagens `root.writable`, `store.schema`, `dns.*`, `tls.*`, `port.<n>`, `runtime.vcredist`, `path.user` — entradas do Forgebox no PATH do usuário —, `defender.exclusion` — raiz excluída do Defender, lida com `Get-MpPreference`; só administradores veem a lista —, `versions.daemon`, `registry.*`, `service.*`) e acrescenta `versions.cli` (forge × daemon × API). Sai com 1 se alguma checagem falhou (avisos não contam). Checagens com correção automática trazem `fix_method` (e `fix_params`) no `--json`: `dns.policy.<tld>` → `net.dns_enable`, `tls.ca_trusted` → `tls.ca_install {scope: current_user}`, `path.user` → `shell.path_add`; a GUI mostra o botão "Corrigir" e roda de novo a checagem. `--export <arquivo>` grava um pacote de suporte em JSON, montado no cliente só com métodos públicos (funciona também contra um daemon remoto): `{v, created_unix, forge_version, api_version, doctor, status, tools, daemon_log}` — resultado do doctor, `daemon.status`, versões instaladas por ferramenta e as últimas 500 linhas do log do daemon (`logs.read`; se o log falhar, uma linha avisa e o resto é gravado).
O daemon em si é `forgeboxd [--root <dir>] [--foreground] [--log-level <filtro>]` (`--listen` é do modo remoto, M6-E3, e `--self-update <arquivo>` do M4-E1: ambos recusados com código 2 por enquanto). Saída 0 = parada limpa, 4 = já existe um daemon nesta raiz (mutex `Global\Forgebox.Daemon[-<chave>]`, mensagem pt-BR no stderr), 1 = falha no boot. Ctrl+C, Ctrl+Break, fechar o console e `daemon.stop` fazem o mesmo shutdown gracioso; `daemon.restart` faz o shutdown e inicia um daemon novo ([ADR-0029](adr/0029-daemon-composition.md)).

### tool / catálogo
```
forge search <texto> [--category db]              # busca no(s) registry(ies); sem texto lista na ordem do índice
forge tool list [--installed] [--updates]
forge tool info php                                # versões, variantes, política, sites que usam
forge tool info php --versions [--channel preview] # releases dos providers: série → patches, builds, tamanhos (offline: cache)
forge tool info php --history [--series 8.3] [--limit N]   # updates e rollbacks, do mais novo ao mais antigo (padrão 50, máx. 500)
forge tool install php@8.3 [--variant ts] [--yes]  # série → última patch; php@8.3.36 → exata
forge tool install nginx mysql@8.4 mailpit         # vários
forge tool install php@8.4 nginx --dry-run         # só o plano: o que seria baixado e o total (≈ com estimativas)
forge tool install xdebug [--for php@8.3]          # extensão: na versão indicada (padrão: em todas as compatíveis)
forge tool remove php@8.2.29 [--keep-config]
forge tool remove xdebug@3.4.1 [--for php@8.4]     # extensão: só dessa versão
forge tool update [php|--all] [--check]            # respeita políticas; --check só lista; sem alvo e sem --all é erro 2 (--check sozinho lista tudo)
forge tool rollback php@8.3                        # volta para a versão retida da série
forge tool policy php auto-patch|notify|manual
forge tool add-local <zip|dir>                     # manifesto embutido ou discover
forge tool discover                                # varre bin/ por pastas não registradas
forge use php 8.4 | node 22 | python 3.13          # troca junction `current` (CLI global)
forge which php                                    # mostra versão/caminho ativo (global e do projeto atual)
```

### service / instância
```
forge start [all|nginx|mysql/default|php@8.3]      # php@8.3 = pool
forge stop [...] | forge restart [...] | forge reload nginx
forge status [--watch]                             # tabela com estado, portas, CPU/RAM, uptime
forge service create mysql@8.4 legacy --port 3307 [--data-dir D:\dados\mysql]   # nova instância
forge service remove mysql/legacy [--delete-data]
forge service config nginx                          # lista os arquivos e, num terminal, abre no editor (FORGE_EDITOR/VISUAL/EDITOR; Notepad)
forge service set php@8.3 memory_limit=1G upload_max_filesize=256M   # quick settings
forge service set mysql/default                     # sem pares: os ajustes com valores atuais e padrões, e as portas (ports.<nome>)
forge service update mysql/default [--autostart|--no-autostart] [--arg <a>]... [--no-args] [--env K=V]... [--no-env] [--restart never|failure|always] [--restart-max N] [--restart-window <s>] [--version 8.4.14]
forge service ports                                 # mapa de portas e donos (inclui conflitos externos)
forge service kill-port 80 [--pid 4412]             # encerra o processo de fora do Forgebox que ocupa a porta (pede confirmação; recusa Forgebox, serviços, sistema e outros usuários: docs/11 §4)
forge logs <alvo> [-f] [-n N] [--since 10m] [--grep x] [--before <seq>]    # nginx | nginx.error | php@8.3 | mysql/default | site:loja.test | app:loja.test/web
forge logs --list                                   # alvos disponíveis (logs.targets)
```
- `forge logs` lê os arquivos do alvo **inclusive os rotacionados** (`.out.log.1` … `.4`; o log do daemon, todos os dias guardados). Cada linha tem um `seq` monotônico por alvo ([ADR-0046](adr/0046-logs-seq-domain.md)): o índice absoluto da linha no conjunto de arquivos (log do daemon: `dia << 32 + linha`), o mesmo no backlog, nas linhas ao vivo e depois de reinícios do daemon ou rotações — é o que o `--json` mostra em `seq`.
- `--list` lista os alvos (`logs.targets`: alvo, tipo, grupo, formato, ao vivo; `--json` traz também o arquivo relativo à raiz, para `fs.tail`): o daemon; cada instância (`mysql/default`) e cada log declarado no manifesto (`nginx.error`, `nginx.access`: `service.daemon.logs[]`, com o `format`); os pools de PHP com pasta de log (`php@8.4`); um `site:<host>` por site cujo web server tem access log; um `app:<host>/<processo>` por processo de site. `--before <seq>` pagina para trás: as `-n` linhas mais novas com `seq` menor, então páginas seguidas (`--before` = o menor `seq` da anterior) são contíguas e sem sobreposição; com `--grep`/`--since` (e em `site:`) o daemon varre até 100 000 linhas para preencher a página.
- `site:<host>` lê o access log JSON do web server do site (o mapeamento `logs[].access` do manifesto, lido pelo parser do `forge-metrics`, [14 §3.1](14-observability-metrics.md)) e mantém só as linhas cujo host é o do site ou um alias (sem porta, em minúsculas). Cada linha traz `access` (host, status, método, URI como registrada, tempo em ms, upstream); o nível vem do status (5xx `error`, 4xx `warn`). O `seq` é o índice da linha no access log compartilhado, então as linhas de um site têm lacunas. Acompanha anexos e sobrevive a truncamento. Sem access log para ler (web server sem instância), `not_found`.
- `app:<host>/<processo>` lê o log da instância do AppManager do processo ([ADR-0028](adr/0028-app-processes-launch-path-on-visit.md); `logs/apps/<host>/<processo>.log`) e segue o arquivo para as linhas novas; processo ou site desconhecido → `not_found`. Esse alvo passa por um redator básico: o valor de `Authorization:` (com o esquema `Bearer`/`Basic`) e de `password=`/`passwd=` (também `"password": "..."`) vira `***`, e o `--grep` olha o texto já redigido ([11 §6](11-security.md)).
- `<tool>@<série>` (pool) lê o primeiro log que o manifesto declara em `service.pool.logs[]` (o `php.yaml` declara o `error_log` do `php.ini.j2`, `logs/php/<série>/error.log`, com `format: php`); sem log declarado, ou enquanto o arquivo não existe, lê o log de worker mais recente da pasta, sem parser (só texto). O alvo vem da lista de logs do manifesto, nunca de um `if tool == php`.
- Nível e hora de cada linha vêm do parser do `format` do manifesto (`log_format.rs`, escolhido pelo formato e nunca pela tool): `nginx-error`, `apache-error`, `php`, `mysql` (MySQL e MariaDB), `postgresql` e `json` (o log do próprio daemon). Níveis normalizados em `error`/`warn`/`info`/`debug`; horas sem fuso (nginx, Apache, MariaDB) são do horário local da máquina. Linhas que não casam (as de um stack trace do PHP) ficam sem hora e nível.
- `service set <alvo>` sem pares lê (`services.settings`: tabela chave/valor/padrão/aplicação e as portas `ports.<nome>`); com `chave=valor` grava (`services.set`, um job; valores em texto: `true`/`false`, números decimais, itens de `multi` separados por `,`; `chave=` volta ao padrão; portas com `ports.<nome>=<porta>`). Alvos: uma instância (`mysql/default`, o ULID, ou a tool com uma instância só) ou uma série de PHP (`php@8.4`; `php@8.4.27` vale a série; `php` sozinho se houver uma série só). Tudo é validado antes de gravar: valor inválido → `invalid_input` (saída 2) nomeando a chave; bloco gerenciado editado à mão → `conflict`; porta ocupada → `port_in_use` (saída 4) com `pid`/`exe`. Os valores vão para o bloco gerenciado do arquivo da tool ou, no PHP, para `etc/php/<série>/conf.d/90-forgebox-settings.ini` (o `php.ini` nunca é editado); `reload` recicla o pool, `restart` reinicia pelo supervisor e uma porta nova reinicia a instância nela ([04 §1.3](04-tool-manifest-spec.md), [ADR-0042](adr/0042-quick-settings-storage-and-apply.md)). `service update` muda as configurações avançadas de uma instância ([ADR-0041](adr/0041-ipc-additions-m2-core-screens.md)): o que não vem fica como está; `--arg`/`--env` substituem a lista inteira (`--no-args`/`--no-env` limpam); `--version` só aceita outra versão instalada da mesma série (os dados não são migrados). A instância é re-renderizada e reiniciada se estiver rodando.
- `logs.subscribe` com `tail` num alvo que descarta linhas (`site:` com o log compartilhado de vários sites, ou `--grep`) varre uma janela larga de linhas cruas (até 100 000, fora do runtime) e envia as `n` últimas linhas aceitas; o formato e o filtro rodam em `spawn_blocking`, nunca numa thread do runtime.
- `-f` mostra as últimas `-n` linhas (padrão 50, no máximo 10 000) e segue ao vivo. Se a conexão cai (daemon reiniciado), o `forge` reconecta e retoma do último `seq` recebido: as linhas escritas nesse meio-tempo são reenviadas dos arquivos; as que a rotação já apagou (ou mais de 10 000 atrás) viram o aviso de linhas perdidas (`logs.lagged`), nunca uma lacuna silenciosa.

### php
```
forge php list | forge php pool status [8.3]
forge php ext list 8.3 | forge php ext enable 8.3 intl | disable
forge php xdebug 8.3 on|off|trigger [--port 9003]
forge php ini 8.3 [--open]                          # caminho / abre no editor (num terminal)
```
- `php ext list <versão>` mostra o estado real da série (M2-E2-T9): as DLLs de `ext/` (embutidas, ligadas por `defaults_on` e pelas escolhas guardadas) e as extensões do registry instaladas naquela versão (Xdebug: não embutida, `zend`, desligada até ser ligada). `php ext enable|disable <versão> <nome>` é um job: DLL que não existe → `not_found` (saída 3) sem nada gravado; a escolha vale para a série (`etc/php/<série>/conf.d/10-forgebox.ini`) e os pools da série que rodam são reciclados só se algo mudou. Voltar ao padrão não deixa nada guardado.
- `php xdebug <versão> on|trigger [--port N]` instala o Xdebug que falta na versão dentro do mesmo job (com progresso), reserva a porta do cliente (padrão 9003; dono `php@<série>/xdebug` em `forge service ports`), grava o preset do modo (`on`: `start_with_request = yes`; `trigger`: `trigger`; ambos `mode = debug,develop`) em `conf.d/20-xdebug.ini` e recicla o pool; `off` desliga a linha e mantém a reserva e a DLL. Porta com listener (de fora) → `port_in_use` (saída 4) com `pid`/`exe` e nada gravado; reservada para outro dono → `conflict`. Rodar de novo com o mesmo modo não muda nada. Com uma série (`8.4`) e um pool da série rodando, `php ext …` e `php xdebug` agem sobre a versão que serve (é com ela que os arquivos da série são renderizados); uma versão exata que não serve, enquanto a que serve não tem Xdebug, → `conflict` nomeando a que serve. O Xdebug por site (`SiteXdebug`) fica fora do M2.

### site
```
forge site list [--json]
forge site add <dir> [--host loja.test] [--php 8.3] [--server nginx] [--secure] [--driver laravel]
forge site remove loja.test                          # pede confirmação (--yes sem terminal); nunca apaga arquivos do projeto
forge site php loja.test 8.4                        # troca ao vivo
forge site secure loja.test | unsecure
forge site server loja.test apache
forge site driver loja.test laravel|auto            # fixa o driver do framework; auto detecta de novo  # sites parked: conflict (seguem a detecção; use site add)
forge site alias loja.test add admin.loja.test
forge site open loja.test [--editor|--terminal|--explorer]
forge site info loja.test                           # diagnóstico: site.dns, site.certificate (aviso < 30 dias), site.php_pool, site.root_access
forge site env loja.test [--reveal]                 # .env/.env.local da pasta do projeto, segredos mascarados no daemon (--reveal: só admin; docs/11 §6)
forge site pool loja.test shared|dedicated          # pool PHP compartilhado (CPU/RAM ≈) ou exclusivo (exatos, upstream php_<ver>__<slug>); job
forge site metrics loja.test on|off                 # liga/desliga as métricas do site (desligadas por padrão em raízes portáteis)
forge park <dir> [--tld .test] | forge unpark <dir> | forge parked
forge link [<dir>] [--host] | forge unlink [<dir>]    # equivalente valet; unlink acha o site pela pasta
forge proxy add app.test http://127.0.0.1:3000 [--ws] [--secure]
forge proxy remove app.test
forge up [<dir>]                                    # lê forgebox.yaml (kind: Project), prepara tudo e sobe processes
forge down [<dir>]
forge ps [loja.test]                                # processes do site
forge run loja.test web|queue start|stop|restart|logs [-f]
forge share loja.test [--provider cloudflared|ngrok|expose] [--stop]
forge new laravel <nome> [--php 8.4] [--db mysql]   # stacks: cria projeto, site, banco, .env
```

### db
```
forge db list [mysql/default]
forge db create loja [--on mysql/default] [--charset utf8mb4]
forge db drop loja --yes
forge db export loja [--on mysql/default] [--output D:\dumps\loja.sql.zst]   # job; padrão backups\mysql\default\loja-<UTC>.sql.zst
forge db import loja arquivo.sql[.gz|.zst] [--on mysql/default]               # job; Ctrl+C cancela
forge db shell mysql/default [loja]                  # roda o cliente CLI neste console (--json: só imprime o comando)
forge db url mysql/default loja                      # imprime DSN/URL para .env
forge db open mysql/default --with heidisql|dbeaver|adminer|phpmyadmin   # M3-E1
```
`db shell` recebe do daemon um `ShellCommandDto {program, args, cwd, env}` ([ADR-0040](adr/0040-e3-api-terminal-db-mail-logs.md)) e o executa preso ao console, saindo com o código do cliente; a GUI abre o mesmo comando num terminal integrado. O que é banco de dados vem do bloco `spec.database` do manifesto (`db list --json` traz `instances` com protocolo e capacidades).

Comportamento do backend (M2-E3-T5, `crates/forge-daemon/src/databases.rs`; tudo vem de `spec.database`, sem código por tool):
- **Instância:** `--on`/primeiro argumento aceitam ULID, `tool/nome` ou `tool` (a instância `default` ou a única); sem referência vale a única instância de banco (ou a `default`). Sem nenhuma ferramenta de banco instalada, `db list` devolve listas vazias e `create`/`drop`/`url`/`shell` respondem `not_found`. Uma instância parada responde `conflict` com a dica ``forge start mysql/default`` (`db url` e `db shell` não precisam do servidor ligado; `db list` sem instância informada só pula as paradas e as devolve em `instances`).
- **Validação antes de qualquer substituição:** nome `^[A-Za-z0-9_]{1,64}$`, charset `^[A-Za-z0-9_]{1,32}$`; nome inválido é `invalid_input` e o cliente nunca é iniciado. Bancos de sistema (`system_databases`) não aparecem em `db list` e `create`/`drop` os recusam (`invalid_input`). `db drop` sem `confirm: true` (CLI: sem `--yes`) é `invalid_input`.
- **Nomes diferenciam maiúsculas de minúsculas** (docs/15): `Loja` e `loja` são bancos distintos; o filtro de bancos de sistema compara texto exato.
- **Execução:** o cliente (`client.exe` + `args` com `{{ sql }}`) roda pelo probe runner (Job Object, timeout de 5 s, sem janela, dentro de `spawn_blocking`) e a saída TSV é lida na 1ª coluna. `db.list`, `db.create` e `db.drop` têm prazo de resposta de 8 s (o padrão de 1 s é para chamadas que não saem do daemon). Falha do cliente vira `conflict` (banco já existe), `not_found` (banco inexistente) ou `conflict` com o fim do `stderr`.
- **Tamanho:** `instances[].data_size_bytes` vem de uma varredura em cache da pasta de dados (TTL 60 s, em thread bloqueante); é `null` até a primeira terminar, então `db list` responde em menos de 1 s mesmo a frio. `DatabaseDto` traz só nome e instância (charset, tabelas e tamanho por banco ainda não: `queries.list` devolve apenas o nome).
- **`db shell [banco]`:** `ShellCommandDto` com o caminho absoluto do cliente interativo e `cwd` na pasta da versão; o banco é acrescentado como último argumento (família `mysql`) ou substitui o valor de `-d` (família `postgresql`).
- **`db export` / `db import`** (M2-E3-T6, `crates/forge-daemon/src/databases/transfer.rs`): jobs (`job_id` na hora, `job.progress`, `jobs.cancel`/Ctrl+C), que rodam os clientes `spec.database.dump` (`mysqldump`, `pg_dump`) e `restore` (`mysql`, `psql`) no runner one-shot de streaming do `forge-supervisor` (`oneshot.rs`: Job Object próprio aninhado no Job raiz, stdin/stdout/stderr bombeados ao mesmo tempo, canais limitados de 16 × 64 KiB — no máximo 1 MiB em trânsito; disco ou servidor lento só deixa o job mais lento). Nome, comando, caminho e estado são validados antes de existir o job (`invalid_input`/`not_found`/`conflict` na resposta).
  - **Caminhos:** absolutos na máquina do daemon (`--output`/arquivo relativo → `invalid_input`; a CLI já converte o relativo pelo diretório atual). O nome escolhe o formato: `.sql.zst` (zstd nível 3), `.sql.gz` (gzip; na importação, todos os membros) ou `.sql` (sem compressão), sem diferenciar maiúsculas na extensão; outro → `invalid_input`. Cliente **remoto** (ADR-0009) só importa de dentro de `www\` e só exporta para dentro de `www\` ou `backups\` (caminho resolvido com junctions/links antes de comparar; fora → `forbidden`); local usa qualquer caminho.
  - **Export:** sem `--output`, `backups\<tool>\<instância>\<banco>-<UTC aaaammddThhmmssZ>.sql.zst` ([08 §2](08-release-layout.md)). A saída do dump vai, comprimida numa thread bloqueante, para `<saída>.partial`, que recebe `fsync` e só então é renomeado (substituindo um arquivo de mesmo nome). Progresso: bytes sem compressão e velocidade média (total desconhecido); o resultado é `{path, size_bytes}` (tamanho do arquivo final). Falha ou cancelamento mata o Job do cliente e apaga o `.partial`.
  - **Import:** o arquivo é lido e descomprimido numa thread bloqueante e vai para o stdin do `restore`; `pct` = bytes lidos do arquivo / tamanho (monotônico, 100 no fim). Bancos de sistema são recusados. Arquivo corrompido → `invalid_input`, e o cliente é morto **antes** de o stdin fechar (nunca recebe um fim de arquivo que o faria aplicar meio dump como se fosse inteiro). Cancelar no meio deixa o banco com o que já foi aplicado (sem transação global; importe num banco vazio).
  - **Falha do cliente** (saída ≠ 0): `job.failed` com as **últimas 20 linhas do stderr** na mensagem (`not_found` quando é banco inexistente, senão `conflict`).
  - **Medido** (Windows 10 19045, RDP, debug; `crates/forge-daemon/tests/databases.rs`): dump falso de 64 MiB → `.sql.zst` (21,5 MB) em ~3,3–4,2 s (o zstd de debug não é otimizado; o binário de release é), sha256 do descomprimido igual, 64 relatórios de progresso; `services.status` com p99 de ~3–12 ms durante a exportação de 64 MiB pelo `forgeboxd` (orçamento 50 ms; acima dele a medição é refeita uma vez, como em `concurrency.rs`); cancelar na metade termina em < 2 s sem processo nem `.partial`. Ida e volta real (`tests/databases_online.rs`, `#[ignore]`): MySQL 8.4.13 e PostgreSQL 17.11, tabela de 1000 linhas → `db.export` → `drop` → `create` → `db.import` → mesma contagem.

### net / tls
```
forge dns status | enable | disable | test loja.test    # test sai com 1 se o nome não resolve como esperado
forge tls ca status | install [--scope user|machine] | uninstall [--scope user|machine] | export [-o ca.crt] | rotate
forge tls issue loja.test | renew [loja.test|--all] | export loja.test [--pfx [--password x]] [-o arquivo]
forge tls list                                      # certificados dos sites: validade (dias restantes) e se este Windows confia neles (`trusted`: assinado pela CA atual e a CA confiada)
forge lan expose loja.test | unexpose | qr loja.test
```

### mail
```
forge mail status | open | list [--unread] [--search texto] [--limit N] [--offset N] | show <id> [--keep-unread] | clear
forge mail show <id> --attachment <parte> [-o arquivo]   # baixa um anexo (até 4 MiB; partes em `mail show <id> --json`)
forge mail read <id>... [--unread] | delete <id>...      # marcar lidas/não lidas, apagar (pede confirmação)
forge mail env                                      # imprime MAIL_* para .env
```
`show` marca a mensagem como lida (`--keep-unread` não). Sem `-o`, um anexo de texto vai para a saída e um binário pede `-o` (o nome do arquivo vem do remetente e nunca é usado para gravar). O backend é escolhido pelo bloco `spec.mail` do manifesto (família `mailpit-v1`), e o daemon publica `mail.changed` quando as contagens mudam (M2-E3-T7). Sem nenhuma tool instalada que declare `spec.mail`, todo `mail ...` responde `not_found` com `forge tool install mailpit`; com a instância parada, `conflict` com `forge start <tool>/<instância>`. O adaptador fala com `127.0.0.1:<porta http>/api/v1` (2 s por requisição, sem proxy); um Mailpit travado vira `timeout` em até 2,5 s sem afetar o resto do daemon. `mail show` lê sem marcar como lida para um cliente `viewer`; `mail.attachment` recusa anexos acima de 4 MiB (`invalid_input` apontando a UI do Mailpit) sem baixá-los inteiros.

### shell / env
```
forge shell [<dir>]                                 # abre shell com PATH do Forgebox e versões do projeto
forge env [<dir>] [--powershell|--cmd|--bash]       # imprime exports para `Invoke-Expression (forge env --powershell)`
forge exec [--php 8.2] -- php artisan migrate       # executa com versão específica sem trocar current
forge path add|remove|show                           # gerencia entradas do PATH do usuário
```

### fs / paridade Linux
```
forge fs case-sensitive status [<dir>] | enable <dir> | disable <dir> | convert <dir> [--revert]
forge site case-sensitive loja.test on|off
forge site lint-case loja.test [--fix [--dry-run] [--strategy auto|rename|edit]] [--json]    # Case Mismatch Finder (docs/15 §6); sai com 1 se restam problemas
```

### jobs
```
forge jobs [--active]                               # operações longas em curso (instalações, exports, starts)
forge jobs watch <job_id> | cancel <job_id>
```

### stats / remote
```
forge stats [loja.test] [--window 1m|5m|1h|24h|7d] [--watch] [--json]   # rps, p50/p90/p99, err%, cpu≈, mem≈; --watch a cada 2/10/60 s
forge stats [loja.test] --from 24h|<RFC 3339> [--to ...]   # intervalo exato (merge dos histogramas; a janela só rotula)
forge stats loja.test --series [--window 1h] [--from ...] [--to ...]   # pontos dos gráficos: passo 2 s (1m/5m), 10 s (1h), 1 min (24h), 1 h (7d)
forge stats loja.test --routes                      # rotas mais lentas (top-K de 12 a 24 h) e erros recentes (4xx/5xx do access log)
forge top                                           # processos do Forgebox por CPU/RAM com dono (site/instance)
forge stats export loja.test --from 24h|<RFC 3339> [--to ...] -o csv|json    # imprime o conteúdo
forge remote add hml wss://10.0.0.5:7788 --code 1234-5678 | forge remote add vps ssh://user@host
forge remote list | use hml | remove hml            # `forge --context hml <qualquer comando>` também funciona
forge remote pair | listen --addr 0.0.0.0:7788 | clients | revoke <id>   # no lado remoto
```

### registry / sistema
```
forge registry list | add <url> | remove <nome> | update
forge import laragon C:\laragon [--dry-run] [--copy-tools] [--move-data] [--convert-case]
forge bundle export php@8.3 nginx -o stack.fbx | forge bundle import stack.fbx
forge backup now [mysql/default] | forge backup schedule daily 02:00
forge config list | get|set <chave> [valor]        # forgebox.toml (list: valores efetivos; --json traz padrões e o JSON Schema)
# config set: gravações passam por um único dono no daemon (sem atualização perdida), são aplicadas ao vivo quando possível
#   (dns.enabled liga/desliga o resolvedor; tld/dns.extra_tlds recarregam zonas, pastas parked e web) e publicam config.changed {keys, requires};
#   `requires` lista o que falta fazer (tls.ca_rotate depois de mudar o TLD) e o `forge` imprime como aviso.
                                                    # terminal.integrated_shell = pwsh|powershell|cmd; terminal.max_sessions = 1..256 (16)
forge self-update [--check] [--channel beta]
forge completions powershell|bash|zsh
forge mcp serve                                      # servidor MCP via stdio para IDEs/agentes
```

## Mapeamento para a API
Cada comando é um método `grupo.acao` da API do daemon ([ADR-0015](adr/0015-ipc-protocol.md)). A tabela canônica é `forge_ipc::api::methods::METHODS` (campo `cli` lista os comandos atendidos), exportada para a GUI em `apps/desktop/src/ipc/methods.ts`; um teste garante que todo comando desta página tem método. Exemplos: `forge site add`/`forge link` → `sites.add` (job) · `forge proxy remove`/`forge unlink` → `sites.remove` · `forge logs [-f]` e `forge daemon logs` → `logs.read`/`logs.subscribe` (alvo `daemon`) · `forge status --watch`, `forge jobs watch` → `events.subscribe` · `forge stats --watch` → `metrics.subscribe` · `forge exec` → `shell.env` com `versions` + execução local · `forge service config` abre o editor (`services.config`, `local_only`); a GUI remota usa `services.config_files` + `fs.read/write`.

Comando equivalente ([ADR-0037](adr/0037-cli-equivalent-templates.md)): cada linha de `METHODS` com `cli` não vazio tem `cli_template`, um template renderizado a partir dos params da chamada (`MethodInfo::render_cli`; na GUI, `src/ipc/cli.ts` a partir do campo `cli_template` de `methods.ts`), que alimenta o "Copiar comando" da GUI. Gramática: `{campo}` posicional, `{campo=--flag}` opção com valor, `{campo?--flag}` flag booleana, `{campo?a:b}` escolha, `{lista...}` variádico, `{a.b}` campo aninhado, e alternativas ` | ` com regra `[campo=valor]` quando um método atende vários comandos (`sites.open --editor`, `shell.env --cmd`, `tls ca install --scope machine`). Valores com espaço ou `\` ficam entre aspas duplas (válidas em PowerShell e `cmd`); o que nenhum shell preserva (`"`, `$`, `` ` ``, `%`, vazio) não gera comando. `confirm`, `yes` e senhas nunca aparecem: a CLI pergunta. Todo comando novo desta página entra com template, stub no clap e uma entrada em `crates/forge-ipc/tests/fixtures/cli_templates.json`; os testes `forge-ipc/tests/cli_templates.rs` (cobertura e golden) e `forge-cli/tests/cli_templates_roundtrip.rs` (parse no clap e volta ao mesmo método) falham se algum faltar.

Rodam só no cliente (sem método): `forge daemon start` (inicia o `forgeboxd`), `forge completions`, `forge mcp serve`, `forge remote add|list|use|remove` e `--context` (perfis de conexão guardados na máquina do cliente).

Sem comando da CLI (só a GUI e o protocolo): `tools.icon` (ícone SVG do catálogo, sanitizado; a GUI o guarda pelo `sha256`, ver abaixo), `net.port_check` (valida o campo de porta), `system.hello`, `system.ping`, `events.*`, `terminal.*` (o terminal integrado: sessões no daemon, `admin`, não `local_only`; o equivalente mostrado é `forge shell`: a sessão tem o mesmo ambiente de `forge shell`/`forge env` — o builder de `shell.env`, com as versões da pasta ou do site, mais `FORGEBOX_TERMINAL=1` —, roda o shell de `terminal.integrated_shell` (`pwsh` → `powershell` → `cmd`; o PowerShell já vem com `forge completions powershell` carregado) e no máximo `terminal.max_sessions` (16) ficam abertas; ver [03 §7.2.1](03-engine.md)) e `shell.open_editor` (abre um arquivo no editor na linha/coluna; `local_only`).

API do E3 ([ADR-0040](adr/0040-e3-api-terminal-db-mail-logs.md), aditiva na 1.1): os métodos novos (`logs.targets` — pronto desde o M2-E3-T8 —, `mail.mark_read|delete|attachment`, `shell.open_editor`, `system.config_list`, `tls.ca_uninstall`, `services.kill_port_holder`, `sites.env`) e os ainda sem backend respondem `not_implemented` citando a tarefa dona (`M2-E3-T8`…`T9`; `db.open` → `M3-E1`) até o backend chegar — `system.hello.not_implemented` os lista. `terminal.*` funcionam desde o M2-E3-T4, `db.list|create|drop|url|shell` desde o M2-E3-T5, `db.export|import` desde o M2-E3-T6, `mail.*` (`status|list|show|mark_read|delete|clear|attachment|env|open`) desde o M2-E3-T7 e `tls.ca_uninstall`, `services.kill_port_holder` e `sites.env` desde o M2-E3-T11 ([ADR-0045](adr/0045-security-extras-ca-uninstall-port-holder-env.md)).

API do E2 ([ADR-0041](adr/0041-ipc-additions-m2-core-screens.md), aditiva na 1.1): `tools.releases` e `tools.plan` (jobs, `viewer`; nada é baixado) funcionam desde o M2-E2-T5 (abaixo), `tools.history` e `tools.icon` desde o M2-E2-T4; `sites.driver` (job, `operator`) e `tls.list` (`viewer`) saíram desse grupo no M2-E2-T6 e funcionam, e `services.settings` (`viewer`; o bloco `advanced` só vai para admin) e `services.set` respondem desde o M2-E2-T7; `tools.install|remove` com `--for` (`target`) funcionam desde o M2-E2-T8 (extensões por versão alvo, [ADR-0043](adr/0043-extension-install.md); `--for` com tool comum é `invalid_input`); `services.update` (job, `admin`) e `service create --data-dir` funcionam desde o M2-E2-T10. `forge tool info <t> --versions [--channel c]` (`tools.releases`) lista as releases por versão, da mais nova à mais antiga: série, canal, builds (variantes, preferida primeiro), tamanho e se está instalada; sem rede mostra o cache de metadados com o aviso "lista em cache (data)" (`stale: true`, `fetched_at` no `--json`), e sem cache nenhum falha com `io` ou `timeout` em até 15 s (cada provider tem 12 s). `forge tool install <specs> --dry-run` (`tools.plan`) resolve os specs com o mesmo código do install (série → último patch, variante padrão, `depends_on` ainda não instalados primeiro, extensão no `--for`) e mostra o que seria baixado, o total (≈ com estimativas do manifesto) e os itens sem tamanho; não escreve nada em `bin/`. Ambos são jobs: `jobs.cancel` os encerra na hora, mesmo com um provider lento. O plano de uma stack (`tools.plan {stack}`) e os `args`/`env` de `services.update` (`launch`) não têm comando equivalente: a GUI não mostra comando em vez de mostrar um errado.

`forge service update <instância>` e `service create --data-dir` (`services.update`, `services.create`, M2-E2-T10; [03 §3.5](03-engine.md)): tudo é validado antes de abrir o job. `--arg`/`--env` substituem a lista inteira e `--no-args`/`--no-env` limpam; o ambiente aceita `[A-Za-z_][A-Za-z0-9_]*` (nunca `PATH`), no máximo 64 itens de até 8 KiB; `--restart`/`--restart-max`/`--restart-window` mudam só o que foi dado sobre a política em vigor (`failure` precisa de `--restart-max` >= 1; janela de 1 a 86 400 s). `--version` aceita uma versão **instalada da mesma série** (`8.4.14` → `8.4.13` vale, é um downgrade também); outra série é `invalid_input` (saída 2) explicando que a pasta de dados não é migrada (instale e crie uma instância nova nela) e uma versão da série que não está instalada é `not_found`. `--autostart`/`--no-autostart` sozinhos não reiniciam nada; qualquer mudança de argumentos, ambiente, política ou versão reconstrói o ator e reinicia a instância se ela rodava (parada, só renderiza). Saída: `Instância atualizada: <tool>/<nome>` ou, com `--json`, o `InstanceDto`. O estado atual (`args`, `env`, política e a padrão do manifesto, versões da mesma série) está em `advanced` de `services.settings` (`forge service set <alvo> --json`; só para admin). `service create --data-dir <pasta>` exige caminho absoluto, sem `.`/`..`, que não seja raiz de unidade, a raiz do Forgebox (nem um pai dela), nada dentro de `bin/` (as versões são trocadas nos updates) e uma pasta sem sobreposição com a de outra instância, nos dois sentidos (igual, dentro dela ou que a contenha) e que não seja `data/` nem `data/<tool>` (`invalid_input`/`conflict` antes do job); a pasta é usada como está (não é movida, `data.init` roda nela se ainda não existir) e `service remove --delete-data` nunca apaga uma pasta escolhida com `--data-dir` (só apaga a pasta gerenciada em `data/`). `InstanceDto.data_dir` mostra a pasta e `size_bytes` vem da varredura periódica (TTL 60 s, em segundo plano; `null` até a primeira).

Metadados do catálogo (M2-E2-T4, sem rede: tudo vem dos manifestos verificados no cache):

- `tools.search` (`forge search`) e `tools.info` (`forge tool info`) preenchem `kind` (`tool|extension|stack`), `summary`, `color` (`ui.color`), `icon_sha256`, `platforms` e `native` (a plataforma do daemon está em `spec.platforms`), `official`, `latest` (do índice), `size_estimate_bytes` (`install.size_estimate_mb`; stacks: soma das tools) e `update_available` (**offline**: o `latest` do índice quando ele pertence a uma série instalada com versão mais velha; a resposta com provider é a de `tool update --check`). Texto vazio lista o catálogo na **ordem do índice** (tools por id, depois as stacks pela ordem curada); com texto vale o ranking. `tools.info` traz também `description_md` (Markdown, não confiável: sem HTML cru), `homepage`, `docs`, `changelog_url` (`metadata.changelog_url`, https), `license`, `variants` com rótulos, `suggests`, `extends` (extensões) e `stack.tools` (stacks: `forge tool info laravel` lista as tools; stacks não têm versões instaladas). `forge tool info` imprime os links e, para stacks, a tabela de tools.
- `tools.icon` entrega o SVG que o índice assinado lista (`spec.tools[].icon {path, sha256}`; o hash cobre os bytes exatos de `icons/<arquivo>.svg`). O arquivo é verificado como os manifestos (hash diferente recusa o registry inteiro), vai para o cache (`state/registry/<nome>/icons/`) e é servido do cache, inclusive sem a fonte. Antes de sair do daemon passa por `forge_registry::icon::sanitize_svg`: até 64 KiB, UTF-8, raiz `<svg>`, sem `<script>`, `<foreignObject>`, `<iframe>`/`<object>`/`<embed>`, sem atributos `on*`, `href`/`xlink:href` só `#id` ou `data:`, `url()` só `#id`/`data:`, sem `@import`, `javascript:`, DOCTYPE/entidades nem animações que mirem `href`; recusar é `verification_failed` (o SVG aceito é idêntico ao do índice: `icon_sha256` é o hash do que `tools.icon` devolve). Sem ícone (ou URL `https`, que nunca é baixada) é `not_found`.
- `tools.history` (`forge tool info <tool> --history [--series] [--limit]`) mapeia as linhas de `update_history` (docs/05 §4): updates (`user`/`auto`) e rollbacks (`rollback`, com `rollback_of`; o update desfeito fica `rolled_back`), do mais novo ao mais antigo; `--series` filtra pela série gravada. Saída humana: `QUANDO  DE  PARA  ORIGEM  ESTADO  NOTA`; `--json` é o `UpdateHistoryDto`. Tool sem histórico: lista vazia.

API das métricas ([ADR-0044](adr/0044-metrics-v1.md), aditiva na 1.1): `stats.get` ganhou `from`/`to` (`forge stats --from/--to`), `stats.series` (`forge stats <site> --series`) e `stats.routes` (`forge stats <site> --routes`) são `viewer`, `TopDto.machine` traz o monitor da máquina, e `sites.pool_mode` (job, `forge site pool`) e `sites.metrics` (`forge site metrics`) são `operator`. `stats.get` e `metrics.subscribe` (`forge stats`, `forge stats --watch`) têm backend desde o M2-E5-T8; até o backend chegar, os demais respondem `not_implemented` citando a tarefa dona: `stats.series`, `stats.routes`, `stats.top` e `stats.export` → M2-E5-T9; `sites.pool_mode` e `sites.metrics` → M2-E5-T11. Numa raiz portátil as métricas ficam desligadas até `forge config set metrics.enabled true` ([14](14-observability-metrics.md) §3.7).

`forge site open <site>` (`sites.open`, `local_only`): sem opção abre no navegador padrão; `--explorer` abre a pasta do site no Explorer (ambos pelo `ShellOpener` da plataforma, `ShellExecuteExW`); `--editor` e `--terminal` respondem `not_implemented` citando M2-E3-T9 até essa tarefa.

Métodos `local_only` (recusados em conexões remotas): `sites.open`, `db.open`, `db.shell`, `services.config`, `services.kill_port_holder`, `php.ini_open`, `mail.open`, `shell.open`, `shell.open_editor`, `shell.env`, `tools.add_local`. `terminal.open` abre uma sessão **no daemon** e por isso não é `local_only` (exige `admin`).

## Detalhes estruturados de erro
([ADR-0039](adr/0039-error-details-and-shell-api.md), API 1.1.) `ApiError.details` (e `JobErrorInfo.details`, `error.details` do `--json`) segue a convenção `{"reason": "<id estável em snake_case>", "args": {...}}`: a GUI traduz a partir de `code` + `details`, nunca da `message` em inglês. Adições são compatíveis; `reason` e as chaves de `args` documentadas aqui não mudam de nome.

| `code` | `reason` | `args` |
|---|---|---|
| `port_in_use` | `port_in_use` | `port`, `owner` (`mysqld.exe (PID 4412)` ou `null`), `pid` e `exe` do processo que escuta (ou `null`; API 1.1, [ADR-0042](adr/0042-quick-settings-storage-and-apply.md)); `port` e `owner` também no nível de cima (forma da API 1.0) |
| `needs_elevation` | `needs_elevation` | `detail` (operação que pediu o helper) |
| `not_found` | `not_found` | `what` (o que não existe, em inglês: texto de apoio, não para traduzir) |
| `not_implemented` | `not_implemented` | `what`; `method` e `task` quando o método ou parte dele é de uma tarefa futura (`sites.open` com `with=editor` → `task: "M2-E3-T9"`) |
| `timeout` | `timeout` | `operation`, `after_ms` |
| `not_found` | `version_not_installed` | `tool`, `requested`, `installed` (versões instaladas da tool), `fix` (comando que instala); `sites.php` com uma versão ou série não instalada — o site não muda (implementado no M2-E2-T6, [ADR-0041](adr/0041-ipc-additions-m2-core-screens.md)) |
| (falha de serviço) | `kind` do diagnóstico (`port_in_use`, `missing_dll`, `missing_runtime`, `config_invalid`, `dependency_not_running`, `permission`, `health_timeout`, `exited_during_start`, `crash_loop`, `other`) | os demais campos do `FailureReasonDto`, que continuam no nível de cima (API 1.0) |

Os demais códigos não têm `details` (a GUI mostra o `code` e a `message`). Implementação: `ForgeError::details()` (`forge-core`) alimenta `ApiError::from` e `JobErrorInfo::from`; `ApiError::with_reason`, `reason()` e `args()` montam e leem a convenção. `system.hello` traz `not_implemented: [métodos]` (a tabela `NOT_IMPLEMENTED` do daemon) para a GUI desabilitar o que ainda não existe sem tentar; `daemon.status` traz `cpu_pct` e `rss_bytes` do próprio daemon.

## Comportamentos
- Erros da API trazem um `code` estável (`forge_core::error::ForgeError::code`, também no `--json`) que define o código de saída (`forge_core::error::exit_code_for`): `invalid_input`, `config_invalid` → 2 · `not_found` → 3 · `conflict`, `port_in_use` → 4 · `needs_elevation` → 5 · `verification_failed` → 6 · `io`, `db`, `unsupported_platform`, `timeout`, `not_implemented`, `cancelled`, `internal`, `unauthorized` (token inválido), `forbidden` (papel insuficiente, método `local_only` em conexão remota, caminho fora de `fs.*`) → 1. Códigos nunca são renomeados; só se acrescentam.
- `--yes` pula confirmações; sem TTY assume `--yes=false` e falha com código 2 em ações que exigem confirmação (CI deve passar `--yes`). Pedem confirmação: `site remove`, `proxy remove`, `unlink`, `tool remove`, `service remove`, `service kill-port`, `db drop`, `tls ca rotate`, `mail clear`, `mail delete`, `remote revoke` (o daemon também exige `confirm: true`). Com terminal, a pergunta é `[s/N]`.
- Ações que precisam do helper elevado (`dns enable|disable`, `tls ca install|uninstall --scope machine`, `lan expose|unexpose`) mostram "Precisa de confirmação de administrador (UAC)…" no stderr e disparam o prompt; com `--no-elevate` falham com código 5 **antes** de qualquer requisição (a API não tem flag de elevação: é um pré-voo do cliente). Se o daemon responder `needs_elevation` por outro motivo, o código de saída também é 5.
- **Daemon automático** ([ADR-0030](adr/0030-cli-contract.md)): se o pipe não responde e não há `--no-daemon`, `forge` avisa no stderr ("Iniciando o daemon…", `--quiet` silencia), inicia `forgeboxd.exe` (ao lado do `forge.exe`, senão no `PATH`; `FORGE_DAEMON_EXE` força um caminho) destacado e sem herdar os handles do `forge`, e espera a API responder por até 10 s (código 1, `timeout`, se não vier). Se o daemon iniciado terminar antes de responder, o erro sai na hora (código 1, `io`, com o código de saída dele e a pasta de logs), exceto a saída 4 (outro daemon ganhou o mutex e está subindo), que conta como sucesso e a espera continua. Com `--no-daemon` e sem daemon: código 1. A mecânica fica em `forge_ipc::client::local`, compartilhada com o `Forgebox.exe`.
- Operações longas (`install`, `update`, `site add`, `doctor`...) devolvem um job: o `forge` assina `job.*` e `tool.install.*` antes de chamar, desenha barras com velocidade/ETA num terminal (nada com `--json`/`--quiet` ou sem terminal), confere `jobs.get` a cada segundo por segurança e termina com o resultado do job; falha de job sai com o `code` do erro do job. Ctrl+C pede `jobs.cancel` e sai com 1 (`cancelled`).
- Downloads mostram barra multi-item com velocidade/ETA; `--quiet` só imprime resultado (em comandos que criam algo, só o identificador: `site add -q` imprime o host).
- Toda tabela tem `--json`: o documento é o DTO da API tal como veio (ex.: `forge status --json` = `{"instances":[],"pools":[]}`); streams (`logs -f`, `status --watch`, `stats --watch`) imprimem JSON lines. **Erros no `--json`** saem em stdout como `{"error":{"code":"not_found","message":"...","details":{}}}` (stdout é sempre JSON válido nesse modo; o texto humano de erro vai para o stderr). Erros de linha de comando do clap também (`invalid_input`, código 2).
- Cores só em terminal: `NO_COLOR` as desliga, `CLICOLOR_FORCE` as liga em pipes (`NO_COLOR` vence). Mensagens em pt-BR por padrão; `FORGE_LANG=en` troca para en-US (as mensagens que vêm da API são em inglês e não mudam).
- `forge exec -- cmd args` roda o comando com o `PATH` das versões do projeto (ou `--php`) sem trocar `current` e sai com o código do filho. O ambiente inclui `bin/shims` no fim do `PATH` e o `config.env` das versões escolhidas (`PHPRC`, `PHP_INI_SCAN_DIR`: o `php` da CLI usa o mesmo `php.ini` dos pools); com `--php` também `FORGEBOX_PHP_VERSION`, que os shims respeitam (`forge exec --php 8.3 -- composer install` roda o Composer no PHP 8.3). O programa é procurado no `PATH` do filho com o `PATHEXT`, como num shell (`composer` → `bin/shims/composer.cmd`). `forge env` imprime o script (`--powershell` padrão, `--cmd`, `--bash`, com o `PATH` e as variáveis; com `--json` os dados). Ver [06 §2.3](06-sites-vhosts-multi-php.md).
- Perfis de conexão (`forge remote add|list|use|remove`) ficam na máquina do cliente (`%LOCALAPPDATA%\Forgebox\remotes.toml`), não no daemon; o código de pareamento (`--code`) é de uso único e não é guardado. O modo remoto é o M6-E3: com um perfil remoto selecionado (`remote use` ou `--context`), os comandos que falam com o daemon dizem `not_implemented` (código 1) em vez de usar o daemon local; `remote use local` volta.
- `forge mcp serve` entrega stdin/stdout ao `forge-mcp.exe` (ao lado do `forge.exe` ou no `PATH`); sem ele: `not_implemented`.
- Autocomplete gerado por clap para PowerShell/bash/zsh (e fish/elvish); `forge completions powershell | Out-String | Invoke-Expression`. Não precisa de daemon.
- Exemplo completo "do zero":
  ```powershell
  forge tool install php@8.4 nginx mysql@8.4 mailpit
  forge dns enable; forge tls ca install
  forge new laravel loja --php 8.4
  forge site open loja.test
  ```
