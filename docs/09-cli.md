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
forge doctor                      # diagnóstico completo: PATH, DNS, CA, portas, vcredist, Defender, versões
```
`daemon status|start|stop|restart` nunca iniciam o daemon só para perguntar (ou parar) algo: `status` de um daemon parado diz "parado" e sai com 1 (`{"running": false}` no `--json`); `stop` de um daemon parado sai com 0; `restart` de um parado o inicia. `stop` e `restart` só voltam quando o pipe sumiu / o novo daemon responde. `doctor` roda o job `daemon.doctor` (checagens `root.writable`, `store.schema`, `dns.*`, `tls.*`, `port.<n>`, `runtime.vcredist`, `path.user` — entradas do Forgebox no PATH do usuário —, `defender.exclusion` — raiz excluída do Defender, lida com `Get-MpPreference`; só administradores veem a lista —, `versions.daemon`, `registry.*`, `service.*`) e acrescenta `versions.cli` (forge × daemon × API). Sai com 1 se alguma checagem falhou (avisos não contam).
O daemon em si é `forgeboxd [--root <dir>] [--foreground] [--log-level <filtro>]` (`--listen` é do modo remoto, M6-E3, e `--self-update <arquivo>` do M4-E1: ambos recusados com código 2 por enquanto). Saída 0 = parada limpa, 4 = já existe um daemon nesta raiz (mutex `Global\Forgebox.Daemon[-<chave>]`, mensagem pt-BR no stderr), 1 = falha no boot. Ctrl+C, Ctrl+Break, fechar o console e `daemon.stop` fazem o mesmo shutdown gracioso; `daemon.restart` faz o shutdown e inicia um daemon novo ([ADR-0029](adr/0029-daemon-composition.md)).

### tool / catálogo
```
forge search <texto> [--category db]              # busca no(s) registry(ies)
forge tool list [--installed] [--updates]
forge tool info php                                # versões, variantes, política, sites que usam
forge tool info php --versions [--channel preview] # releases dos providers: série → patches, builds, tamanhos (offline: cache)
forge tool info php --history [--series 8.3] [--limit N]   # updates e rollbacks
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
forge service kill-port 80 [--pid 4412]             # encerra o processo de fora do Forgebox que ocupa a porta (pede confirmação)
forge logs <alvo> [-f] [-n N] [--since 10m] [--grep x] [--before <seq>]    # nginx | nginx.error | php@8.3 | mysql/default | site:loja.test | app:loja.test/web (os dois últimos: M2-E3-T8)
forge logs --list                                   # alvos disponíveis (logs.targets)
```
- `forge logs` lê os arquivos do alvo **inclusive os rotacionados** (`.out.log.1` … `.4`; o log do daemon, todos os dias guardados). Cada linha tem um `seq` monotônico por alvo ([ADR-0046](adr/0046-logs-seq-domain.md)): o índice absoluto da linha no conjunto de arquivos (log do daemon: `dia << 32 + linha`), o mesmo no backlog, nas linhas ao vivo e depois de reinícios do daemon ou rotações — é o que o `--json` mostra em `seq`.
- `--list` lista os alvos (`logs.targets`: alvo, tipo, grupo, formato, ao vivo; M2-E3-T8). `--before <seq>` pagina para trás: só linhas com `seq` menor (até M2-E3-T8 responde `not_implemented`, em vez de devolver as mais novas de novo).
- `service set <alvo>` sem pares lê (`services.settings`: tabela chave/valor/padrão/aplicação e as portas `ports.<nome>`); com `chave=valor` grava (`services.set`, um job; valores em texto: `true`/`false`, números decimais, itens de `multi` separados por `,`; `chave=` volta ao padrão; portas com `ports.<nome>=<porta>`). Alvos: uma instância (`mysql/default`, o ULID, ou a tool com uma instância só) ou uma série de PHP (`php@8.4`; `php@8.4.27` vale a série; `php` sozinho se houver uma série só). Tudo é validado antes de gravar: valor inválido → `invalid_input` (saída 2) nomeando a chave; bloco gerenciado editado à mão → `conflict`; porta ocupada → `port_in_use` (saída 4) com `pid`/`exe`. Os valores vão para o bloco gerenciado do arquivo da tool ou, no PHP, para `etc/php/<série>/conf.d/90-forgebox-settings.ini` (o `php.ini` nunca é editado); `reload` recicla o pool, `restart` reinicia pelo supervisor e uma porta nova reinicia a instância nela ([04 §1.3](04-tool-manifest-spec.md), [ADR-0042](adr/0042-quick-settings-storage-and-apply.md)). `service update` muda as configurações avançadas de uma instância ([ADR-0041](adr/0041-ipc-additions-m2-core-screens.md)): o que não vem fica como está; `--arg`/`--env` substituem a lista inteira (`--no-args`/`--no-env` limpam); `--version` só aceita outra versão instalada da mesma série (os dados não são migrados). A instância é re-renderizada e reiniciada se estiver rodando.
- `-f` mostra as últimas `-n` linhas (padrão 50, no máximo 10 000) e segue ao vivo. Se a conexão cai (daemon reiniciado), o `forge` reconecta e retoma do último `seq` recebido: as linhas escritas nesse meio-tempo são reenviadas dos arquivos; as que a rotação já apagou (ou mais de 10 000 atrás) viram o aviso de linhas perdidas (`logs.lagged`), nunca uma lacuna silenciosa.

### php
```
forge php list | forge php pool status [8.3]
forge php ext list 8.3 | forge php ext enable 8.3 intl | disable
forge php xdebug 8.3 on|off|trigger [--port 9003]
forge php ini 8.3 [--open]                          # caminho / abre no editor (num terminal)
```

### site
```
forge site list [--json]
forge site add <dir> [--host loja.test] [--php 8.3] [--server nginx] [--secure] [--driver laravel]
forge site remove loja.test                          # pede confirmação (--yes sem terminal); nunca apaga arquivos do projeto
forge site php loja.test 8.4                        # troca ao vivo
forge site secure loja.test | unsecure
forge site server loja.test apache
forge site driver loja.test laravel|auto            # fixa o driver do framework; auto detecta de novo
forge site alias loja.test add admin.loja.test
forge site open loja.test [--editor|--terminal|--explorer]
forge site info loja.test                           # diagnóstico: DNS, cert, pool, root, driver
forge site env loja.test [--reveal]                 # .env/.env.local do site com segredos mascarados (--reveal: só admin)
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
forge db export loja [-o loja.sql.zst] | forge db import loja arquivo.sql[.gz|.zst]
forge db shell mysql/default [loja]                  # roda o cliente CLI neste console (--json: só imprime o comando)
forge db url mysql/default loja                      # imprime DSN/URL para .env
forge db open mysql/default --with heidisql|dbeaver|adminer|phpmyadmin   # M3-E1
```
`db shell` recebe do daemon um `ShellCommandDto {program, args, cwd, env}` ([ADR-0040](adr/0040-e3-api-terminal-db-mail-logs.md)) e o executa preso ao console, saindo com o código do cliente; a GUI abre o mesmo comando num terminal integrado. O que é banco de dados vem do bloco `spec.database` do manifesto (`db list --json` traz `instances` com protocolo e capacidades).

### net / tls
```
forge dns status | enable | disable | test loja.test    # test sai com 1 se o nome não resolve como esperado
forge tls ca status | install [--scope user|machine] | uninstall [--scope user|machine] | export [-o ca.crt] | rotate
forge tls issue loja.test | renew [loja.test|--all] | export loja.test [--pfx [--password x]] [-o arquivo]
forge tls list                                      # certificados dos sites: validade e se este Windows confia neles
forge lan expose loja.test | unexpose | qr loja.test
```

### mail
```
forge mail status | open | list [--unread] [--search texto] [--limit N] [--offset N] | show <id> [--keep-unread] | clear
forge mail show <id> --attachment <parte> [-o arquivo]   # baixa um anexo (até 4 MiB; partes em `mail show <id> --json`)
forge mail read <id>... [--unread] | delete <id>...      # marcar lidas/não lidas, apagar (pede confirmação)
forge mail env                                      # imprime MAIL_* para .env
```
`show` marca a mensagem como lida (`--keep-unread` não). Sem `-o`, um anexo de texto vai para a saída e um binário pede `-o` (o nome do arquivo vem do remetente e nunca é usado para gravar). O backend é escolhido pelo bloco `spec.mail` do manifesto (família `mailpit-v1`), e o daemon publica `mail.changed` quando as contagens mudam (M2-E3-T7).

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
forge stats [loja.test] [--window 5m|1h|24h|7d] [--watch] [--json]   # rps, p50/p90/p99, err%, cpu≈, mem≈
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
forge self-update [--check] [--channel beta]
forge completions powershell|bash|zsh
forge mcp serve                                      # servidor MCP via stdio para IDEs/agentes
```

## Mapeamento para a API
Cada comando é um método `grupo.acao` da API do daemon ([ADR-0015](adr/0015-ipc-protocol.md)). A tabela canônica é `forge_ipc::api::methods::METHODS` (campo `cli` lista os comandos atendidos), exportada para a GUI em `apps/desktop/src/ipc/methods.ts`; um teste garante que todo comando desta página tem método. Exemplos: `forge site add`/`forge link` → `sites.add` (job) · `forge proxy remove`/`forge unlink` → `sites.remove` · `forge logs [-f]` e `forge daemon logs` → `logs.read`/`logs.subscribe` (alvo `daemon`) · `forge status --watch`, `forge jobs watch` → `events.subscribe` · `forge stats --watch` → `metrics.subscribe` · `forge exec` → `shell.env` com `versions` + execução local · `forge service config` abre o editor (`services.config`, `local_only`); a GUI remota usa `services.config_files` + `fs.read/write`.

Comando equivalente ([ADR-0037](adr/0037-cli-equivalent-templates.md)): cada linha de `METHODS` com `cli` não vazio tem `cli_template`, um template renderizado a partir dos params da chamada (`MethodInfo::render_cli`; na GUI, `src/ipc/cli.ts` a partir do campo `cli_template` de `methods.ts`), que alimenta o "Copiar comando" da GUI. Gramática: `{campo}` posicional, `{campo=--flag}` opção com valor, `{campo?--flag}` flag booleana, `{campo?a:b}` escolha, `{lista...}` variádico, `{a.b}` campo aninhado, e alternativas ` | ` com regra `[campo=valor]` quando um método atende vários comandos (`sites.open --editor`, `shell.env --cmd`, `tls ca install --scope machine`). Valores com espaço ou `\` ficam entre aspas duplas (válidas em PowerShell e `cmd`); o que nenhum shell preserva (`"`, `$`, `` ` ``, `%`, vazio) não gera comando. `confirm`, `yes` e senhas nunca aparecem: a CLI pergunta. Todo comando novo desta página entra com template, stub no clap e uma entrada em `crates/forge-ipc/tests/fixtures/cli_templates.json`; os testes `forge-ipc/tests/cli_templates.rs` (cobertura e golden) e `forge-cli/tests/cli_templates_roundtrip.rs` (parse no clap e volta ao mesmo método) falham se algum faltar.

Rodam só no cliente (sem método): `forge daemon start` (inicia o `forgeboxd`), `forge completions`, `forge mcp serve`, `forge remote add|list|use|remove` e `--context` (perfis de conexão guardados na máquina do cliente).

Sem comando da CLI (só a GUI e o protocolo): `tools.icon` (ícone SVG sanitizado do catálogo, guardado pela GUI pelo `sha256`), `net.port_check` (valida o campo de porta), `system.hello`, `system.ping`, `events.*`, `terminal.*` (o terminal integrado: sessões no daemon, `admin`, não `local_only`; o equivalente mostrado é `forge shell`) e `shell.open_editor` (abre um arquivo no editor na linha/coluna; `local_only`).

API do E3 ([ADR-0040](adr/0040-e3-api-terminal-db-mail-logs.md), aditiva na 1.1): os métodos novos (`terminal.*`, `logs.targets`, `mail.mark_read|delete|attachment`, `shell.open_editor`, `system.config_list`, `tls.ca_uninstall`, `services.kill_port_holder`, `sites.env`) e os de bancos/e-mail respondem `not_implemented` citando a tarefa dona (`M2-E3-T4`…`T11`; `db.open` → `M3-E1`) até o backend chegar — `system.hello.not_implemented` os lista.

API do E2 ([ADR-0041](adr/0041-ipc-additions-m2-core-screens.md), aditiva na 1.1): `tools.releases` e `tools.plan` (jobs, `viewer`; nada é baixado), `tools.history`, `tools.icon`, `services.update` (job, `admin`), `sites.driver` (job, `operator`) e `tls.list` respondem `not_implemented` citando a tarefa dona (`M2-E2-T4`…`T10`) até o backend chegar (`services.settings` — `viewer`; o bloco `advanced` só vai para admin — e `services.set` já respondem desde M2-E2-T7); `tools.install|remove` com `--for` (`target`) citam M2-E2-T8 e `service create --data-dir` cita M2-E2-T10, em vez de ignorar o parâmetro. O plano de uma stack (`tools.plan {stack}`) e os `args`/`env` de `services.update` (`launch`) não têm comando equivalente: a GUI não mostra comando em vez de mostrar um errado.

`forge site open <site>` (`sites.open`, `local_only`): sem opção abre no navegador padrão; `--explorer` abre a pasta do site no Explorer (ambos pelo `ShellOpener` da plataforma, `ShellExecuteExW`); `--editor` e `--terminal` respondem `not_implemented` citando M2-E3-T9 até essa tarefa.

Métodos `local_only` (recusados em conexões remotas): `sites.open`, `db.open`, `db.shell`, `services.config`, `services.kill_port_holder`, `php.ini_open`, `mail.open`, `shell.open`, `shell.open_editor`, `shell.env`, `tools.add_local`. `terminal.open` abre uma sessão **no daemon** e por isso não é `local_only` (exige `admin`).

## Detalhes estruturados de erro
([ADR-0039](adr/0039-error-details-and-shell-api.md), API 1.1.) `ApiError.details` (e `JobErrorInfo.details`, `error.details` do `--json`) segue a convenção `{"reason": "<id estável em snake_case>", "args": {...}}`: a GUI traduz a partir de `code` + `details`, nunca da `message` em inglês. Adições são compatíveis; `reason` e as chaves de `args` documentadas aqui não mudam de nome.

| `code` | `reason` | `args` |
|---|---|---|
| `port_in_use` | `port_in_use` | `port`, `owner` (`mysqld.exe (PID 4412)` ou `null`), `pid` e `exe` do processo que escuta (ou `null`; API 1.1, [ADR-0042](adr/0042-quick-settings-storage-and-apply.md)); `port` e `owner` também no nível de cima (forma da API 1.0) |
| `needs_elevation` | `needs_elevation` | `detail` (operação que pediu o helper) |
| `not_found` | `not_found` | `what` (o que não existe, em inglês: texto de apoio, não para traduzir) |
| `not_implemented` | `not_implemented` | `what`; `method` e `task` quando o método ou parte dele é de uma tarefa futura (`sites.open` com `with=editor` → `task: "M2-E3-T9"`; `logs.read` com `before_seq` → `task: "M2-E3-T8"`) |
| `timeout` | `timeout` | `operation`, `after_ms` |
| `not_found` | `version_not_installed` | `tool`, `requested`, `installed` (versões instaladas da tool), `fix` (comando que instala); `sites.php` com uma versão ou série não instalada — o site não muda (M2-E2-T6, [ADR-0041](adr/0041-ipc-additions-m2-core-screens.md)) |
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
