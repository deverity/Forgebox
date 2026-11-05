# ADR-0030 — Contrato do `forge`: cliente fino com daemon automático, `--json` igual ao DTO, erros com código estável, jobs acompanhados por eventos

**Status:** aceito · **Data:** 2026-10-27

## Contexto
[docs/09](../09-cli.md) lista ~130 comandos, todos chamadas da API do daemon (`forge-ipc`, [ADR-0015](0015-ipc-protocol.md)). A tarefa M1-E5-T3 precisava fechar o que o texto deixava aberto:
1. O que o `--json` imprime (e o que acontece em erro) para que scripts e a GUI/MCP dependam disso.
2. Como o `forge` inicia o daemon sem prender o terminal de quem o chamou.
3. Como acompanhar jobs (instalações, exports) sem sondar e sem perder um job que termina antes da resposta.
4. Como cumprir `--no-elevate` se a API não tem flag de elevação.
5. Onde ficam os perfis de `forge remote` (docs/13 §3.2 e M6-E2 diziam `state/remotes/*.toml`, que é dentro de uma raiz).
6. Como testar "cada comando de docs/09" sem uma tabela escrita à mão que fique para trás.

## Decisão
**Cliente fino sobre `forge-ipc`.** Cada comando é uma chamada tipada de `Client` (`client.sites_list(...)`, `ctx.job::<sites_add, SiteDto>(...)`) seguida de um renderizador. Nada de lógica por tool: o CLI não conhece ids. Os tipos base (ids, `ForgeRoot`, códigos de erro, `Timestamp`, `spawn_detached`) vêm de `forge_ipc::base` (re-export de `forge-core`), para o CLI continuar dependendo só de `forge-ipc` (docs/01 §3). A árvore de comandos é um `clap` derivado (`cli.rs`) com as flags globais (`--json --quiet --yes --root --context --no-daemon --no-elevate`) em qualquer posição.

**`--json` é o DTO.** O documento impresso é o resultado do método serializado como veio (`forge status --json` → `{"instances":[],"pools":[]}`); para jobs, o `result` do job. Streams são JSON lines (`LogLineDto`, snapshot de status, `SiteStatsDto`). Erros são `{"error":{"code","message","details"}}` **em stdout** (o texto humano vai para stderr), então stdout é sempre JSON válido nesse modo; erros de uso do clap viram `invalid_input`. O código de saída vem só do `code` (`forge_core::error::exit_code_for`), nunca da mensagem. Cores passam por `anstream` (`NO_COLOR`, `CLICOLOR_FORCE`, terminal); tabelas por `tabled` com larguras que ignoram ANSI; mensagens em `messages.rs` (chaves tipadas `Msg`, pt-BR e en-US com `FORGE_LANG=en`, placeholders conferidos por teste).

**Daemon automático sem herdar o terminal.** Sem resposta no pipe e sem `--no-daemon`, o `forge` inicia `forgeboxd.exe` (ao lado de `forge.exe`, senão `PATH`) e faz poll de conexão a cada 100 ms por até 10 s; a API é a última coisa que o daemon abre (docs/01 §5.1), então conectar = `daemon.ready`. `daemon status|stop|restart` consultam sem iniciar. O `std::process::Command` herda **todo** handle herdável do pai, inclusive o pipe de stdout de `forge ... | more` ou de um harness de testes: o daemon segurava o pipe e o pipeline nunca via EOF (descoberto pelo teste de autostart, que travou). `forge_core::platform::spawn_detached` passou a usar `CreateProcessW` com `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` só com handles do dispositivo `NUL` (`windows/process.rs::create_detached`, mesma técnica do supervisor), mantendo `DETACHED_PROCESS`, novo grupo e saída do Job do chamador quando permitido. O harness de testes falha se o pipe de um `forge` continuar aberto 5 s depois de ele sair. *(M2-E1-T2: a mecânica — `try_connect` Down/Other, `find_daemon_exe` com `FORGE_DAEMON_EXE`, spawn, espera — mudou para `forge_ipc::client::local`, sem texto de usuário, para o `Forgebox.exe` usar a mesma; o spawn passou a vigiar a saída do processo (`spawn_detached_watched`): saída 4 = outro daemon subindo, a espera continua; qualquer outra saída antes de responder falha na hora em vez de esperar os 10 s. O `forge` só traduz os erros.)*

**Jobs por eventos, com rede de segurança.** `ctx.job::<M, R>` abre `events.subscribe` (`job.*`, `tool.install.*`) **antes** de chamar o método — um job que termina antes da resposta chega do buffer do cliente —, filtra pelo `job_id`, atualiza a barra (`indicatif`, só em terminal, nunca com `--json`/`--quiet`) e consulta `jobs.get` a cada segundo caso o stream falhe. O resultado é o `job.done.result`; falha vira `CliError` com o `code` do job (exit 3 continua 3). Ctrl+C pede `jobs.cancel`. Para isso o daemon passou a preservar o código ao esperar outro job (`ForgeError::from_code`; antes um `not_found` de `tools.rollback` virava `internal`).

**Elevação é pré-voo do cliente.** A API não tem parâmetro de elevação e o helper é do daemon. O `forge` anuncia o UAC no stderr antes de `dns enable|disable`, `tls ca install --scope machine` e `lan expose|unexpose`; com `--no-elevate` falha com `needs_elevation` (5) sem chamar o daemon. Se o daemon responder `needs_elevation`, o código de saída é 5 do mesmo jeito.

**Confirmação.** `ctx.confirm`: `--yes`, ou `[s/N]` com terminal, ou erro 2 (docs/09). O daemon continua exigindo `confirm: true`.

**Perfis remotos na máquina do cliente.** `remotes.toml` em `FORGE_CONFIG_DIR` ou `%LOCALAPPDATA%\Forgebox` (nunca dentro da raiz, que pode nem existir num cliente puro), selecionado por `forge remote use` ou `--context`; o código de pareamento não é guardado. Enquanto o modo remoto (M6-E3) não existe, um perfil remoto selecionado faz os comandos dizerem `not_implemented` em vez de falarem com o daemon local. docs/13 §3.2 e o M6-E2 foram ajustados (certificados do M6-E2 ficam em `remotes/<nome>/` ao lado).

**Cobertura por construção.** `tests/commands.rs` roda, contra um `forgeboxd` real em raiz temporária (portátil, DNS desligado, registry offline), uma tabela comando → argumentos mínimos → resultado (`Ok`, `Err(code)` com o exit code, `not_implemented` citando a tarefa do roadmap), uma vez com `--json` (stdout sempre JSON) e outra em modo humano (sem ANSI), cada execução com limite de 30 s e sem pânico. Dois testes derivam a lista de comandos de fontes que não dependem da tabela: a árvore `clap` (toda folha, e grupos com subcomando opcional, precisa de linha) e `forge_ipc::METHODS[].cli` (todo comando atendido por método). Comandos que bloqueiam por natureza (`logs -f`, `status --watch`) são testados à parte em `misc.rs`, lendo a primeira linha e matando o processo.

**`doctor`.** O job `daemon.doctor` ganhou `path.user`, `defender.exclusion` (PowerShell `Get-MpPreference` via `run_probe`, em `spawn_blocking`, 15 s; sem administrador a lista não é visível: `skipped`) e `versions.daemon`; o CLI acrescenta `versions.cli`. Sai com 1 se alguma checagem falhou.

## Alternativas rejeitadas
- **Embrulhar o JSON** (`{"v":1,"data":...}`): quebraria "serde dos DTOs" e a paridade com a GUI/MCP; a versão é a da API (`system.hello`).
- **Erro JSON em stderr:** scripts que capturam só stdout perderiam o motivo. Stdout sempre JSON é mais simples de consumir.
- **`forge status` inicia o daemon, mas `daemon status` também:** perguntar "está rodando?" subindo o daemon mente. Fica a regra "só o que precisa do daemon o inicia".
- **Limpar `HANDLE_FLAG_INHERIT` dos std handles do `forge` durante o spawn:** é global ao processo e disputa com outras threads; a lista de handles é local à chamada.
- **Tabela de testes escrita à mão sem verificação:** novos comandos ficariam sem teste sem ninguém notar; por isso a árvore clap e `METHODS` são a fonte da cobertura.
- **Flag `no_elevate` na API:** o daemon já decide por operação se pede o UAC; um pré-voo no cliente cobre o contrato de docs/09 sem mudar o protocolo. Se a lista de operações elevadas crescer, o certo é marcá-las na tabela `METHODS`.

## Consequências
- Todo comando novo da API precisa de uma variante no `clap`, um módulo em `commands/` e uma linha na tabela de testes; o teste de cobertura falha caso contrário.
- `forge_core::ForgeError::from_code` é a forma de repropagar o erro de um job; `port_in_use` volta como `conflict` (mesmo exit 4).
- `fs_parity.site_case` passou a devolver `SiteDto` (antes o job devolvia o modelo de domínio, que o cliente tipado não lia); os alvos de log `site:<host>` e `app:<host>/<processo>` respondem `not_found`/`not_implemented` (M2-E3) em vez de `invalid_input`.
- Os comandos de streams dependem de Ctrl+C no console (`tokio::signal`); nos testes o processo é morto.
