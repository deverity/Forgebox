# ADR-0021 — PhpPool: workers como instâncias do supervisor, requests estimadas pela tabela TCP, drenagem atrelada ao listener e blue/green com standby

**Status:** aceito · **Data:** 2026-10-27

## Contexto
[ADR-0003](0003-php-fastcgi-pool-manager.md) decidiu um pool FastCGI próprio por versão de PHP (um `php-cgi.exe` por porta, scaling, recycle, warmup, blue/green). Faltava decidir como, com as restrições do projeto: o grafo de crates proíbe `forge-fcgi → forge-net/forge-render/forge-provider` ([01 §3](../01-architecture.md)); concorrência por atores, nada bloqueia ([ADR-0011](0011-actor-concurrency-model.md)); Win32 só em `forge_core::platform`. Fatos medidos nesta máquina (Windows 10 19045, PHP 8.3.36 e 8.4.27 NTS oficiais):
- o `php-cgi` não expõe quantas requisições atendeu; `FCGI_GET_VALUES` devolve só `FCGI_MAX_CONNS=1`, `FCGI_MAX_REQS=1`, `FCGI_MPXS_CONNS=0` e não conta como requisição;
- com `PHP_FCGI_MAX_REQUESTS=N` o `php-cgi` sai sozinho após N requisições e as conexões ainda na fila de `accept` dele são resetadas;
- ao fim de cada requisição o `php-cgi` faz `shutdown(SD_SEND)` e fecha: uma das pontas de loopback fica em `TIME_WAIT` (≥ 30 s; 120 s por padrão) e aparece em `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_CONNECTIONS)`;
- no Windows um `connect` recusado em loopback leva ~2 s (retransmissões de SYN);
- `bind` numa porta com conexões em `TIME_WAIT` funciona; o `php-cgi` usa `SO_REUSEADDR` (pode "roubar" uma porta com listener).

## Decisão
1. **Um ator `PhpPool` por versão**; cada worker é uma instância de `forge-supervisor` (Job, spawn determinístico, parada por `CTRL_BREAK`) sob um `JobTree::nested()` do pool (Job do pool filho da raiz, Job de cada worker filho do Job do pool) e um `ActorSupervisor` com barramento **privado** (`Supervisor::with_jobs/with_actors`): os workers não viram "serviços" na GUI; o pool assina o barramento privado para saber de saídas na hora. Restart `never`: quem substitui é o pool.
2. **Requests por worker estimadas pela tabela TCP.** A cada tick (1 s; < 30 s obrigatório) uma leitura `PortTable::connections(portas)` (nova primitiva em `forge-core`: todas as conexões das portas, qualquer estado, as duas pontas). Cada ponto final de cliente visto pela primeira vez é uma requisição; a chave é o ponto do cliente, venha da ponta do servidor ou da do cliente, então funciona quem quer que feche primeiro. Conexões que já existiam na porta antes do spawn (restos de um worker anterior na mesma porta) são a linha de base ignorada. A mesma leitura dá `ESTABLISHED` por worker (scaling) e o pool lê working set (`ProcessStats`) e se o processo vive.
3. **Recycle proativo; `PHP_FCGI_MAX_REQUESTS` vira rede de segurança.** O pool troca o worker ao passar de `max_requests` estimado (ou `max_rss_mb`, `max_age`): sobe o substituto, aquece, tira o antigo do upstream na mesma mudança que põe o novo, drena e mata. O ambiente do worker leva `PHP_FCGI_MAX_REQUESTS = max(4 × max_requests, max_requests + 1000)` (sobrescreve o manifesto), para o `php-cgi` nunca sair sozinho sob carga enquanto o substituto aquece.
4. **Upstream por listener injetado, drenagem atrelada à aplicação.** O pool emite `UpstreamModel {name, backends, policy}` (evento `pool.scaled`, `watch`) e chama um `UpstreamListener` do daemon (re-render com `forge-render` + reload). Um worker que sai fica `Retiring` até o listener aplicar uma mudança sem ele; depois `Draining`: `drain_grace` (1 s) para requisições roteadas antes do reload e parada quando `drain_quiet` (2) amostras seguidas não mostram conexão estabelecida nem nova, ou em `drain_timeout` (30 s). Falha do listener → nova tentativa com backoff; o worker antigo continua servindo.
5. **Warmup obrigatório**: tentativas de `connect` de 250 ms até escutar (evita os ~2 s do connect recusado), então `FCGI_GET_VALUES` + GET de `<ini_dir>/_forgebox/health.php` (escrito pelo pool), exigindo 200, o `PHP_VERSION` esperado e que o pid que respondeu seja o do worker.
6. **Blue/green com standby.** O pool alvo sobe em *standby* (aquecido, sem chamar o listener) — numa atualização de patch os dois pools compartilham `php_8_3` —, o pool de origem vai a standby (responde só quando nenhuma chamada do listener dele está em voo), o listener aplica `Swapped{from, to}`, o alvo é promovido e a origem é drenada. Falha antes da aplicação devolve a origem ao serviço e para um alvo iniciado pela troca. Em standby o pool não recicla nem escala.
7. **Portas por trait em `forge-core`** (`model::port::PortReservations`), implementado por `forge_net::ports::PortAllocator`; dono `pool/<version id>#worker-<slot>`.
8. **`PoolSwitcher` não é definido aqui.** `forge_fcgi::Switcher::switch(from, to: VersionId)` (pools resolvidos por um `PoolDirectory` do daemon) é a operação que o motor de updates (M1-E2-T5) precisa; o daemon implementa o trait do motor delegando a ela.

## Alternativas rejeitadas
- **Confiar só em `PHP_FCGI_MAX_REQUESTS = max_requests`:** o `php-cgi` sai no limite e reseta a fila; sob carga, erros visíveis (um `php-cgi` novo leva ~0,3–0,4 s para subir e aquecer nesta máquina, tempo em que um worker atende dezenas de requisições).
- **`PHP_FCGI_MAX_REQUESTS = 0`:** sem rede de segurança para um worker que o pool não conseguisse reciclar (tabela TCP indisponível, por exemplo).
- **Contar só `ESTABLISHED` por amostragem:** requisições de milissegundos quase nunca aparecem; o recycle por requisições não aconteceria.
- **Contar pelo access log do servidor web (`up`/`$upstream_addr`, docs/14):** exato, mas depende do `forge-metrics` (M2/M3), de servidor web rodando e de log habilitado; pode alimentar o pool no futuro sem mudar a decisão.
- **`auto_prepend_file` com contador em memória compartilhada:** invasivo (altera o PHP do usuário) e por versão.
- **Um `InstanceActor` próprio de pool sem `forge-supervisor`:** duplicaria spawn, Job Objects, captura de stdio e parada em cascata.
- **Workers no barramento principal:** cada recycle publicaria 4+ eventos `service.*` de "instâncias" efêmeras na GUI e na tabela `events`.
- **Drenar após um tempo fixo depois do reload:** mata worker com fila; **drenar sem esperar o listener:** o servidor web ainda pode estar roteando para ele.
- **Pool alvo servindo desde o start na troca de patch:** o start do alvo já reescreveria `php_8_3` antes da troca, e o scaling da origem poderia reescrevê-lo de volta depois.
- **Definir o trait `PoolSwitcher` em `forge-core` agora:** fixaria a interface do motor de updates antes da tarefa dele; o `Switcher` concreto cobre o caso e a ligação fica no daemon.

## Consequências
- A estimativa conta conexões, não requisições PHP: as sondas do pool entram na conta (recycle um pouco antes) e uma conexão abortada com RST (sem `TIME_WAIT`) escapa. Com servidor web na mesma máquina (sempre: workers só escutam em loopback) as duas pontas são visíveis.
- A leitura da tabela TCP cresce com as entradas em `TIME_WAIT` (milhares sob carga alta); custa alguns ms por tick em `spawn_blocking`. O `tcp_table` do Windows ganhou mais folga e tentativas para tabelas que crescem entre as duas chamadas.
- `forge-core`: `PortTable::connections` (implementação padrão `NotImplemented` para tabelas falsas), `TcpConnection`, `TcpState`, `model::port::PortReservations`; `forge-supervisor`: `JobTree::nested`, `Supervisor::with_jobs/with_actors/process_stats/port_table`; `forge-ipc`: campos opcionais em `PoolScaledPayload`/`PoolWorkerRecycledPayload` (aditivos, `v` = 1).
- O daemon (M1-E5-T2) monta o `PoolSpec` do manifesto (deixando `{{ port }}` para o pool), injeta `PortAllocator` e o listener (mapeando para `forge_render::model::UpstreamModel` e recarregando o servidor) e mantém a tabela de pools (`PoolDirectory`).
- O e2e com nginx + `oha` fica para o smoke de M1; aqui os clientes FastCGI dos testes fazem o papel do servidor web.
