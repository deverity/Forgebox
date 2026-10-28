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
forge tool install php@8.3 [--variant ts] [--yes]  # série → última patch; php@8.3.36 → exata
forge tool install nginx mysql@8.4 mailpit         # vários
forge tool remove php@8.2.29 [--keep-config]
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
forge service create mysql@8.4 legacy --port 3307  # nova instância
forge service remove mysql/legacy [--delete-data]
forge service config nginx                          # lista os arquivos e, num terminal, abre no editor (FORGE_EDITOR/VISUAL/EDITOR; Notepad)
forge service set php@8.3 memory_limit=1G upload_max_filesize=256M   # quick settings
forge service ports                                 # mapa de portas e donos (inclui conflitos externos)
forge logs <alvo> [-f] [-n N] [--since 10m] [--grep x]    # nginx | nginx.error | php@8.3 | mysql/default | site:loja.test | app:loja.test/web (os dois últimos: M2-E3)
```

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
forge site alias loja.test add admin.loja.test
forge site open loja.test [--editor|--terminal|--explorer]
forge site info loja.test                           # diagnóstico: DNS, cert, pool, root, driver
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
forge db shell mysql/default [loja]                  # abre cliente CLI
forge db url mysql/default loja                      # imprime DSN/URL para .env
forge db open mysql/default --with heidisql|dbeaver|adminer|phpmyadmin
```

### net / tls
```
forge dns status | enable | disable | test loja.test    # test sai com 1 se o nome não resolve como esperado
forge tls ca status | install [--scope user|machine] | export [-o ca.crt] | rotate
forge tls issue loja.test | renew [loja.test|--all] | export loja.test [--pfx [--password x]] [-o arquivo]
forge lan expose loja.test | unexpose | qr loja.test
```

### mail
```
forge mail status | open | list [--unread] | show <id> | clear
forge mail env                                      # imprime MAIL_* para .env
```

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
forge config get|set <chave> [valor]               # forgebox.toml
forge self-update [--check] [--channel beta]
forge completions powershell|bash|zsh
forge mcp serve                                      # servidor MCP via stdio para IDEs/agentes
```

## Mapeamento para a API
Cada comando é um método `grupo.acao` da API do daemon ([ADR-0015](adr/0015-ipc-protocol.md)). A tabela canônica é `forge_ipc::api::methods::METHODS` (campo `cli` lista os comandos atendidos), exportada para a GUI em `apps/desktop/src/ipc/methods.ts`; um teste garante que todo comando desta página tem método. Exemplos: `forge site add`/`forge link` → `sites.add` (job) · `forge proxy remove`/`forge unlink` → `sites.remove` · `forge logs [-f]` e `forge daemon logs` → `logs.read`/`logs.subscribe` (alvo `daemon`) · `forge status --watch`, `forge jobs watch` → `events.subscribe` · `forge stats --watch` → `metrics.subscribe` · `forge exec` → `shell.env` com `versions` + execução local · `forge service config` abre o editor (`services.config`, `local_only`); a GUI remota usa `services.config_files` + `fs.read/write`.

Comando equivalente ([ADR-0037](adr/0037-cli-equivalent-templates.md)): cada linha de `METHODS` com `cli` não vazio tem `cli_template`, um template renderizado a partir dos params da chamada (`MethodInfo::render_cli`; na GUI, `src/ipc/cli.ts` a partir do campo `cli_template` de `methods.ts`), que alimenta o "Copiar comando" da GUI. Gramática: `{campo}` posicional, `{campo=--flag}` opção com valor, `{campo?--flag}` flag booleana, `{campo?a:b}` escolha, `{lista...}` variádico, `{a.b}` campo aninhado, e alternativas ` | ` com regra `[campo=valor]` quando um método atende vários comandos (`sites.open --editor`, `shell.env --cmd`, `tls ca install --scope machine`). Valores com espaço ou `\` ficam entre aspas duplas (válidas em PowerShell e `cmd`); o que nenhum shell preserva (`"`, `$`, `` ` ``, `%`, vazio) não gera comando. `confirm`, `yes` e senhas nunca aparecem: a CLI pergunta. Todo comando novo desta página entra com template, stub no clap e uma entrada em `crates/forge-ipc/tests/fixtures/cli_templates.json`; os testes `forge-ipc/tests/cli_templates.rs` (cobertura e golden) e `forge-cli/tests/cli_templates_roundtrip.rs` (parse no clap e volta ao mesmo método) falham se algum faltar.

Rodam só no cliente (sem método): `forge daemon start` (inicia o `forgeboxd`), `forge completions`, `forge mcp serve`, `forge remote add|list|use|remove` e `--context` (perfis de conexão guardados na máquina do cliente).

Métodos `local_only` (recusados em conexões remotas): `sites.open`, `db.open`, `db.shell`, `services.config`, `php.ini_open`, `mail.open`, `shell.open`, `shell.env`, `tools.add_local`.

## Comportamentos
- Erros da API trazem um `code` estável (`forge_core::error::ForgeError::code`, também no `--json`) que define o código de saída (`forge_core::error::exit_code_for`): `invalid_input`, `config_invalid` → 2 · `not_found` → 3 · `conflict`, `port_in_use` → 4 · `needs_elevation` → 5 · `verification_failed` → 6 · `io`, `db`, `unsupported_platform`, `timeout`, `not_implemented`, `cancelled`, `internal`, `unauthorized` (token inválido), `forbidden` (papel insuficiente, método `local_only` em conexão remota, caminho fora de `fs.*`) → 1. Códigos nunca são renomeados; só se acrescentam.
- `--yes` pula confirmações; sem TTY assume `--yes=false` e falha com código 2 em ações que exigem confirmação (CI deve passar `--yes`). Pedem confirmação: `site remove`, `proxy remove`, `unlink`, `tool remove`, `service remove`, `db drop`, `tls ca rotate`, `mail clear`, `remote revoke` (o daemon também exige `confirm: true`). Com terminal, a pergunta é `[s/N]`.
- Ações que precisam do helper elevado (`dns enable|disable`, `tls ca install --scope machine`, `lan expose|unexpose`) mostram "Precisa de confirmação de administrador (UAC)…" no stderr e disparam o prompt; com `--no-elevate` falham com código 5 **antes** de qualquer requisição (a API não tem flag de elevação: é um pré-voo do cliente). Se o daemon responder `needs_elevation` por outro motivo, o código de saída também é 5.
- **Daemon automático** ([ADR-0030](adr/0030-cli-contract.md)): se o pipe não responde e não há `--no-daemon`, `forge` avisa no stderr ("Iniciando o daemon…", `--quiet` silencia), inicia `forgeboxd.exe` (ao lado do `forge.exe`, senão no `PATH`; `FORGE_DAEMON_EXE` força um caminho) destacado e sem herdar os handles do `forge`, e espera a API responder por até 10 s (código 1, `timeout`, se não vier). Com `--no-daemon` e sem daemon: código 1.
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
