# ADR-0028 — Processes de sites (Procfile 2.0): lançamento sem PATH do usuário, `cmd.exe` por variável de ambiente, `$PORT` na faixa `apps`, `on-visit` como API idempotente

**Status:** aceito · **Data:** 2026-10-27

## Contexto
[06 §4.2](../06-sites-vhosts-multi-php.md) promete "Next.js em `https://app.test` com HMR, zero config": o site declara `processes:`, o Forgebox escolhe a porta, monta o `PATH` da versão certa do runtime, sobe o processo, aponta o vhost para ele e, com `autostart: on-visit`, só sobe quando alguém acessa. Quatro problemas não estavam resolvidos:

1. O supervisor só inicia executáveis reais (`.bat`/`.cmd` são recusados: a citação de argumentos do `cmd.exe` não é segura de montar), mas `npm`, `npx`, `pnpm` e `yarn` são `.cmd`, e uma linha de Procfile pode ter `&&`, `>` ou `%VAR%`.
2. "Nada de lógica por tool em Rust": o código não pode saber que `npm` pertence ao Node, nem que o Node precisa de `NODE_EXTRA_CA_CERTS`.
3. O gatilho do `on-visit` é uma requisição HTTP que o web server devolve como 502; o servidor HTTP interno é do daemon (M1-E5), não deste crate.
4. Um processo que não escuta (fila, scheduler) não deve gastar porta; um que escuta tem de manter a mesma porta entre reinícios, ou o vhost renderizado fica errado.

## Decisão
**Dados, não código por tool.** O que um runtime injeta vem do manifesto: `install.path` (pastas no PATH), `install.shims` (os comandos que ele fornece: `npm` ⇒ `nodejs`), `install.version_files` (`.nvmrc`, `.node-version`, `.php-version`) e `config.env`. O manifesto `nodejs` ganhou `config.env` com `NODE_EXTRA_CA_CERTS: "{{ paths.certs }}/ca/forgebox-ca.crt"` e `npm_config_prefix: "{{ paths.data }}/node/{{ version }}/npm-global"`; o `RuntimeCatalog` (`forge_sites::apps::runtime`) lê isso e nada mais. Variáveis aceitas em tempo de execução: `version`, `series`, `tool` e `paths.<dir>` (sem `paths.version`: a versão é uma pasta de `bin/`).

**Versão do runtime**, a primeira que valer: `runtime: tool@seletor` do processo → arquivo de versão do projeto (do `cwd` do processo até a pasta do projeto, o mais próximo vence, arquivo vazio não decide) → `spec.runtimes[]` do `forgebox.yaml` → link `current` → a mais nova instalada. Seletor `22`, `22.12`, `v22.12.0` = a mais nova instalada que começa com esses números; sem correspondência é erro `not_found` listando o instalado, nunca outra versão em silêncio. Quando o processo não declara `runtime:`, o runtime é o da ferramenta que fornece a primeira palavra do comando.

**PATH determinístico** (docs/03 §1.2): pastas do runtime (`install.path` sob `bin/<tool>/<pasta>`), depois `extra_path` do site (dependências como `bin/vcredist`), depois a pasta do sistema que o supervisor acrescenta. Nunca o PATH do usuário. O executável do comando é procurado só nessas pastas (`.exe`, `.com`, `.cmd`, `.bat`); não achar é `not_found` com a lista de pastas.

**Dois modos de lançamento** (`apps::command`): *direto* quando a primeira palavra é `.exe`/`.com` achado no PATH acima (argumentos separados pelas regras do MSVCRT, que a camada de plataforma cita de volta de forma idêntica) e *shell* quando é `.cmd`/`.bat`, um interno do `cmd.exe` (`echo`, `set`...) ou a linha usa `& | < > ^ %` fora de aspas. No modo shell o processo é `cmd.exe /d /c %FORGEBOX_COMMAND%` e a linha viaja na variável `FORGEBOX_COMMAND`: o `cmd.exe` a expande sozinho, então aspas e operadores chegam intactos e **nenhum argumento é citado** (medido: `npm run dev -- --flag "two words"` entrega `["--flag", "two words"]` ao script). Custo: `CTRL_BREAK` no grupo do `cmd.exe` pode abrir "Terminate batch job?" (stdin é `NUL`); a parada de um app usa `timeout` de 5 s e cai no Job, que mata a árvore inteira.

**Porta.** Pedida por `port: auto|N`; sem `port:`, é `auto` quando o processo tem `proxy: true` ou menciona `$PORT` no comando ou no env, senão nenhuma. `auto` vem de `PortReservations::allocate("apps", {App, <site id>, <processo>})` (idempotente: mesma porta após reinício, vhost sempre válido); fixa vem de `PortReservations::reserve` (novo método **opcional** do trait, `NotImplemented` por padrão, implementado por `PortAllocator`). `$PORT`/`${PORT}` são substituídos no comando e nos valores de `env`; usar `$PORT` sem porta é erro. `PORT` e `FORGEBOX_SITE` entram no ambiente. Instância com porta ganha health TCP em `127.0.0.1:<porta>` (`Running` = escutando; `start_timeout` 120 s: o primeiro build do Next.js é lento) e a pré-checagem de porta.

**Supervisão.** Um `InstanceActor` por processo (tool `app`, nome `<host>.<processo>`), todos num Job aninhado por site (`JobTree::nested`): derrubar o site mata só a árvore dele. Logs em `logs/apps/<host>/<nome>.log` (rotação do supervisor). Um ator `apps` (`AppManager`) guarda o registro e dois flags por processo; trabalho lento (planejamento em `spawn_blocking`, alocação de portas, start/stop) roda na tarefa do chamador. `restart`: `never`/`failure` (padrão do supervisor)/`always`.

**`autostart: on-visit`.** O vhost leva `start_on_visit` (`error_page 502 = /_forgebox/starting`, já nos templates de M1-E4-T1). O servidor HTTP do daemon (M1-E5) chama `AppManager::starting_page(host, lang)`, que chama `ensure_started(host)` — **idempotente: só a primeira chamada por registro inicia** os processos on-visit (sem esperar), as seguintes devolvem `already`, e um processo parado pelo usuário nunca é iniciado por visita — e renderiza `pages/starting.html.j2` (503 com `Retry-After: 2` e refresh de 2 s; 500 sem refresh quando o processo falhou; pt-BR/en-US). O refresh pede de novo a URL original, que o nginx passa a proxiar assim que o app escuta. `Failed` não é repetido a cada visita: a página mostra a falha até o usuário reiniciar.

**Site `kind: app`.** `root` é a pasta do projeto; `processes` (JSON em `sites.processes`, forma de `forgebox.yaml spec.processes`) tem exatamente um `proxy: true`, e o vhost é `proxy_pass http://127.0.0.1:<porta>` com WebSocket ligado por padrão (`ProxyOptions`; `websocket: false` do `forgebox.yaml` é respeitado). `proxy_path` ainda não é renderizado (`not_implemented`).

## Alternativas rejeitadas
- *`cmd.exe /c "<linha>"` com a linha como argumento*: a citação da plataforma escapa aspas internas com `\"`, que o `cmd.exe` não entende; exigiria um modo "linha de comando bruta" no `ProcessCommand` (Win32, outro crate) e continuaria exposto a `^`/`%`.
- *Wrapper `.exe` próprio para `.cmd`*: mais um binário a distribuir e assinar para resolver o que uma variável resolve.
- *Detectar Node por `if tool == "nodejs"`*: viola a regra de manifestos; o `RuntimeCatalog` só consome campos do schema que já existiam (`shims`, `version_files`, `path`, `config.env`).
- *`ensure_started` esperando o app subir*: prenderia uma requisição do daemon por até 2 minutos; a página com refresh já é o feedback e o estado fica visível em `status`.
- *Reservar a porta de processos sem `$PORT`*: consumiria a faixa de 1000 portas com filas e schedulers que não escutam.
