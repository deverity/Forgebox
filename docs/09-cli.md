# 09 — CLI `forge`

`forge` é um cliente fino da API do daemon (`forge-ipc`). Se o daemon não está rodando, `forge` o inicia (exceto `--no-daemon`). Toda saída tem forma humana (tabelas, cores, spinners) e `--json` (estável, versionada). Códigos de saída: 0 ok · 1 erro genérico · 2 uso inválido · 3 recurso não encontrado · 4 conflito (porta/host) · 5 precisa de elevação · 6 verificação falhou.

```
forge <grupo> <comando> [args] [--json] [--quiet] [--yes] [--root <dir>]
```

## Grupos e comandos

### daemon
```
forge daemon status | start | stop | restart | logs [-f] | doctor
forge doctor                      # diagnóstico completo: PATH, DNS, CA, portas, vcredist, Defender, versões
```

### tool / catálogo
```
forge search <texto> [--category db]              # busca no(s) registry(ies)
forge tool list [--installed] [--updates]
forge tool info php                                # versões, variantes, política, sites que usam
forge tool install php@8.3 [--variant ts] [--yes]  # série → última patch; php@8.3.36 → exata
forge tool install nginx mysql@8.4 mailpit         # vários
forge tool remove php@8.2.29 [--keep-config]
forge tool update [php|--all] [--check]            # respeita políticas; --check só lista
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
forge service config nginx                          # abre arquivo no editor
forge service set php@8.3 memory_limit=1G upload_max_filesize=256M   # quick settings
forge service ports                                 # mapa de portas e donos (inclui conflitos externos)
forge logs <alvo> [-f] [--since 10m] [--grep x]    # nginx | nginx.error | php@8.3 | mysql/default | site:loja.test | app:loja.test/web
```

### php
```
forge php list | forge php pool status [8.3]
forge php ext list 8.3 | forge php ext enable 8.3 intl | disable
forge php xdebug 8.3 on|off|trigger [--port 9003]
forge php ini 8.3 [--open]                          # caminho / abre
```

### site
```
forge site list [--json]
forge site add <dir> [--host loja.test] [--php 8.3] [--server nginx] [--secure] [--driver laravel]
forge site remove loja.test
forge site php loja.test 8.4                        # troca ao vivo
forge site secure loja.test | unsecure
forge site server loja.test apache
forge site alias loja.test add admin.loja.test
forge site open loja.test [--editor|--terminal|--explorer]
forge site info loja.test                           # diagnóstico: DNS, cert, pool, root, driver
forge park <dir> [--tld .test] | forge unpark <dir> | forge parked
forge link [<dir>] [--host] | forge unlink           # equivalente valet
forge proxy add app.test http://127.0.0.1:3000 [--ws] [--secure]
forge proxy remove app.test
forge up [<dir>]                                    # lê forgebox.yaml (kind: Project), prepara tudo e sobe processes
forge down [<dir>]
forge ps [loja.test]                                # processes do site
forge run loja.test web|queue start|stop|restart|logs
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
forge dns status | enable | disable | test loja.test
forge tls ca status | install | export [-o ca.crt] | rotate
forge tls issue loja.test | renew [--all] | export loja.test --pfx
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
forge site lint-case loja.test [--fix [--dry-run] [--strategy auto|rename|edit]] [--json]    # Case Mismatch Finder (docs/15 §6)
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
forge stats export loja.test --from ... --to ... -o csv|json
forge remote add hml wss://10.0.0.5:7788 --code 1234-5678 | forge remote add vps ssh://user@host
forge remote list | use hml | remove hml            # `forge --context hml <qualquer comando>` também funciona
forge remote pair | listen --addr 0.0.0.0:7788 | clients | revoke <id>   # no lado remoto
```

### registry / sistema
```
forge registry list | add <url> | remove <nome> | update
forge import laragon C:\laragon
forge bundle export php@8.3 nginx -o stack.fbx | forge bundle import stack.fbx
forge backup now [mysql/default] | forge backup schedule daily 02:00
forge config get|set <chave> [valor]               # forgebox.toml
forge self-update [--check] [--channel beta]
forge completions powershell|bash|zsh
forge mcp serve                                      # servidor MCP via stdio para IDEs/agentes
```

## Mapeamento para a API
Cada comando é um método `grupo.acao` da API do daemon ([ADR-0015](adr/0015-ipc-protocol.md)). A tabela canônica é `forge_ipc::api::methods::METHODS` (campo `cli` lista os comandos atendidos), exportada para a GUI em `apps/desktop/src/ipc/methods.ts`; um teste garante que todo comando desta página tem método. Exemplos: `forge site add`/`forge link` → `sites.add` (job) · `forge proxy remove`/`forge unlink` → `sites.remove` · `forge logs [-f]` e `forge daemon logs` → `logs.read`/`logs.subscribe` (alvo `daemon`) · `forge status --watch`, `forge jobs watch` → `events.subscribe` · `forge stats --watch` → `metrics.subscribe` · `forge exec` → `shell.env` com `versions` + execução local · `forge service config` abre o editor (`services.config`, `local_only`); a GUI remota usa `services.config_files` + `fs.read/write`.

Rodam só no cliente (sem método): `forge daemon start` (inicia o `forgeboxd`), `forge completions`, `forge mcp serve`, `forge remote add|list|use|remove` e `--context` (perfis de conexão guardados na máquina do cliente).

Métodos `local_only` (recusados em conexões remotas): `sites.open`, `db.open`, `db.shell`, `services.config`, `php.ini_open`, `mail.open`, `shell.open`, `shell.env`, `tools.add_local`.

## Comportamentos
- Erros da API trazem um `code` estável (`forge_core::error::ForgeError::code`, também no `--json`) que define o código de saída (`forge_core::error::exit_code_for`): `invalid_input`, `config_invalid` → 2 · `not_found` → 3 · `conflict`, `port_in_use` → 4 · `needs_elevation` → 5 · `verification_failed` → 6 · `io`, `db`, `unsupported_platform`, `timeout`, `not_implemented`, `cancelled`, `internal`, `unauthorized` (token inválido), `forbidden` (papel insuficiente, método `local_only` em conexão remota, caminho fora de `fs.*`) → 1. Códigos nunca são renomeados; só se acrescentam.
- `--yes` pula confirmações; sem TTY assume `--yes=false` e falha com código 2 em ações que exigem confirmação (CI deve passar `--yes`).
- Ações que precisam do helper elevado mostram "Precisa de confirmação de administrador (UAC)…" e disparam o prompt; com `--no-elevate` falham com código 5.
- Downloads mostram barra multi-item com velocidade/ETA; `--quiet` só imprime resultado.
- Toda tabela tem `--json`; eventos em `forge logs -f --json` são JSON lines.
- Autocomplete gerado por clap para PowerShell/bash/zsh; `forge completions powershell | Out-String | Invoke-Expression`.
- Exemplo completo "do zero":
  ```powershell
  forge tool install php@8.4 nginx mysql@8.4 mailpit
  forge dns enable; forge tls ca install
  forge new laravel loja --php 8.4
  forge site open loja.test
  ```
