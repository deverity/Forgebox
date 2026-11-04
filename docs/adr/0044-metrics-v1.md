# ADR-0044 — Métricas v1: mapeamento do access log no manifesto, tail por polling, camadas HDR, atribuição, API (`stats.series`, `stats.routes`, `TopDto.machine`, `sites.pool_mode`, `sites.metrics`) e adiamentos

**Status:** aceito · **Data:** 2026-11-03 · Desenho do épico M2-E5 ([docs/14](../14-observability-metrics.md)). Os detalhes de implementação ficam nas ADRs das tarefas: [ADR-0047](0047-access-log-tailer-identity-checkpoints-sampling.md) (tail) e [ADR-0048](0048-metrics-aggregator-sparse-hdr-tiers-routes.md) (agregador). Segue a [ADR-0009](0009-remote-ready-ipc.md) (API remote-ready), a [ADR-0011](0011-actor-concurrency-model.md) (atores), a [ADR-0015](0015-ipc-protocol.md) (streams), a [ADR-0019](0019-instance-lifecycle-health-restart-metrics.md) (amostragem de processos), a [ADR-0021](0021-php-pool-actor-request-estimation-blue-green.md) (pools PHP) e a [ADR-0029](0029-daemon-composition.md) (composição do daemon). É o PR de contrato do E5 da regra "um PR de contrato por épico, aditivo, na API 1.1" (a [ADR-0039](0039-error-details-and-shell-api.md) subiu a versão; esta mantém 1.1).

## Contexto geral
O docs/14 prometia, por vhost e em tempo real, req/s, p50/p90/p99, erros, bytes, CPU/RAM atribuídos e workers, sem instalar nada no projeto. O desenho original tinha lacunas que impediam implementar o épico em tarefas paralelas:
- um parser por servidor (`if server == nginx`) contra a regra "nada de lógica por tool em Rust";
- `notify` para o tail, que no Windows atrasa eventos de arquivos mantidos abertos por outro processo;
- camadas sem orçamento de memória;
- nenhuma API para gráficos, rotas, monitor da máquina, pool dedicado ou o liga/desliga por site;
- itens caros (instrumentação profunda, consultas lentas, OpenMetrics, toasts) misturados ao v1.

Cada decisão abaixo tem contexto, decisão, alternativas e consequências próprios.

## Decisões

### 1. Formato do access log declarado no manifesto
**Contexto.** nginx, Apache e Caddy escrevem JSON com nomes, unidades e formatos diferentes: segundos em string, µs, `ts` de início ou de fim, host com porta, `ut` com várias tentativas, escapes `\xhh` do Apache.

**Decisão.** O manifesto declara `logs[].access` ([docs/04](../04-tool-manifest-spec.md) §1.2): ponteiros JSON (RFC 6901) com unidade (`s|ms|us|ns`) para timestamp, host, status, duração total, duração do upstream (`multi: sum|last`), bytes, método e URI, mais `start` (o timestamp é a chegada), `strip_query` e `lenient_escapes`. O `forge-metrics` tem um único `AccessParser` guiado por esse mapeamento e não conhece o id da tool. Os templates sempre emitem o log JSON `forgebox` e `FORGEBOX_SITE` ([docs/14](../14-observability-metrics.md) §2).

**Alternativas.**
- Parser por servidor: viola a regra do CLAUDE.md, e cada servidor novo exigiria código.
- Expressões regulares sobre o formato combinado: frágeis com aspas e espaços, e lentas.
- Pedir ao usuário um formato: os templates já são nossos.

**Consequências.** Um servidor novo (FrankenPHP, OpenLiteSpeed) entra só com manifesto e template. O `lint-registry` valida os ponteiros (sem prefixo comum, unidade conhecida). O formato de cada servidor fica testado por fixtures reais (M2-E5-T3).

### 2. Tail por polling e rotação pelo manifesto
**Contexto.** O servidor web mantém o arquivo aberto. Notificações de diretório chegam atrasadas ou agrupadas no Windows. A rotação muda de servidor para servidor: o nginx precisa de rename + `-s reopen`, o Caddy rotaciona sozinho, e o Apache no Windows não reabre por sinal.

**Decisão.**
- Tail por **polling** de 250 ms ± 10 %, com recuo até 1 s sem novidade e 50 ms depois de um tick no teto, só com std, leituras em `spawn_blocking` e no máximo 4 MiB por tick ([ADR-0047](0047-access-log-tailer-identity-checkpoints-sampling.md)).
- A identidade do arquivo é creation time + primeiros 256 bytes. Há checkpoint por lote e amostragem adaptativa marcada como `sampled`.
- A rotação segue `logs[].access.rotate` do manifesto: `rename-reopen` (o daemon renomeia e roda o `reopen` do manifesto pelo caminho de reload da instância; nginx), `self` (o servidor rotaciona pelo template e o tail só acompanha; Caddy `roll_size`/`roll_keep`) ou `none`.
- O Apache é decidido **por experimento** no M2-E5-T12 (hoje `none`, provisório): o resultado medido de cada servidor entra numa tabela no docs/14 §3.

**Alternativas.**
- `notify`/`ReadDirectoryChangesW`: atrasos medidos, e Win32 fora de `platform/` exigiria uma trait só para isso.
- ETW: complexo e exige privilégio.
- Pipe do servidor para o daemon (`access_log syslog:`/pipe): perde linhas quando o daemon reinicia e muda os templates de todos os servidores.
- Rotação com código por servidor: viola a regra das tools.

**Consequências.** Latência de até ~250 ms entre a requisição e a métrica (aceitável para janelas de 2 s). Custo constante e baixo com o arquivo parado. O tail sobrevive a rename, delete + recriação e truncamento, e retoma do checkpoint gravado no SQLite (M2-E5-T7) depois de reiniciar o daemon.

### 3. Camadas de agregação e persistência
**Contexto.** As janelas da UI vão de 60 s a 7 d, e as consultas de intervalo arbitrário (`from`/`to`) precisam de percentis exatos. Um histograma HDR denso custa 84 KiB, então guardá-lo por bucket não cabe na memória.

**Decisão.**
- **Em memória** ([ADR-0048](0048-metrics-aggregator-sparse-hdr-tiers-routes.md)):
  - tier 0 com buckets de **2 s** e histogramas rt/ut por **10 min**;
  - tier 1 com pontos escalares de **10 s** (64 B) por **24 h**;
  - tier 2 com minutos (contagens e percentis) e histogramas **HDR de 1 min** por **24 h**, dentro do orçamento (≥ 1 h garantida), sobre horas com histograma por 24 h.
- **SQLite** (M2-E5-T7, uma migration com o próximo número livre):
  - `metrics_1m`, mantida por **7 dias**;
  - `metrics_1h`, mantida por **90 dias**.

  Cada linha guarda contagens, somas e os histogramas rt e ut como **blobs HDR V2** (sem DEFLATE) com uma coluna **`format_version`**. Flush dos minutos fechados uma vez por minuto, junto com os checkpoints do tail, numa operação da thread de escrita. Rollup por hora, poda diária. Reidratação dos tiers 1 e 2 no boot.
- Consultas acima de 24 h leem do SQLite.

**Alternativas.**
- Tudo no SQLite (sem tiers em memória): uma escrita a cada 2 s por site, e as consultas ao vivo passariam pelo writer.
- Média de percentis por minuto: errada, porque o p99 de uma hora não é a média dos p99 dos minutos.
- t-digest/DDSketch: merge aproximado e erro relativo pior na cauda que o HDR a 3 dígitos. O crate `hdrhistogram` já dá a codificação V2 e o oráculo dos testes.
- Prometheus/TSDB embutido: dependência grande para um app local.

**Consequências.** Memória por site ativo ≈ 1,5 MiB (durações em ms) a 5 MiB (µs). Site ocioso ~1 KiB (medido na ADR-0048). O `format_version` permite trocar a codificação sem migrar dados antigos, porque o leitor aceita as versões conhecidas. Migrations nunca são editadas depois do merge.

### 4. HDR a 3 dígitos, de 1 µs a 10 min, e percentis só de histogramas
**Contexto.** As latências vão de dezenas de µs (estático no Caddy) a minutos (upload, `set_time_limit`). O gate do E5 exige p50/p90/p99 a ±5 % do `oha`.

**Decisão.** Layout HDR do crate `hdrhistogram` com mínimo de 1 µs, máximo de 10 min (valores maiores são limitados ao máximo) e **3 dígitos significativos** (erro ≤ 1/1024). As rotas usam 2 dígitos. Percentis vêm **sempre** de histogramas mesclados (merge exato índice a índice), nunca de médias.

**Alternativas.**
- 2 dígitos para tudo: erro de até 0,8 %, ainda dentro do gate, mas os p99 de rotas e janelas ficariam visivelmente "em degraus" na UI.
- 4 dígitos: 10× mais índices distintos, o que estoura o orçamento de memória.
- Máximo de 1 h: valores acima de 10 min são raros e não mudam o p99.

**Consequências.** O erro máximo (0,1 %) fica muito abaixo dos ±5 % do gate. `p50_ms`/`p90_ms`/`p99_ms` na API são `f64` em ms derivados desses histogramas.

### 5. Atribuição de CPU/RAM por vhost
**Contexto.** Os workers de um pool PHP atendem todos os sites da mesma versão. Os processos de apps (Procfile) e os pools dedicados são de um só site. O servidor web é compartilhado por todos.

**Decisão** (M2-E5-T6, [docs/14](../14-observability-metrics.md) §3.3).
- Pool **compartilhado**: CPU/RAM rateados pela soma do upstream time (`ut`) de cada site no balde de 10 s, ou pelo `rt` quando o servidor não registra `ut`. A API marca `approx = true` e a UI mostra **≈**. Um pool sem tráfego no balde vira overhead e não é atribuído a ninguém.
- Pool **dedicado** e apps: **exatos** (100 % para o dono, `approx = false`).
- **Servidor web: não atribuído.** Ele aparece só em `forge top`/`TopDto` e na parcela do Forgebox no monitor da máquina.
- Trocas no meio do balde (blue/green, mudança de `pool_mode`) são ponderadas pelo tempo.

**Alternativas.**
- Ratear por número de requisições: um site com requisições lentas pagaria o mesmo que um com requisições rápidas.
- Medir CPU por requisição dentro do PHP (`getrusage` via `auto_prepend_file`): instrumentação no projeto, adiada para o M5.
- Atribuir também o servidor web: ele não tem um custo por site mensurável sem instrumentar o servidor.

**Consequências.** Os números por site somam no máximo o custo dos pools e apps. O ≈ é honesto e o usuário que quer exatidão liga o pool dedicado (§7).

### 6. Top-K de rotas e anel de erros recentes em memória; "Erros recentes" do access log
**Contexto.** "Rotas mais lentas" e "Erros recentes" (docs/14 §4.1) precisam de contagem por rota sem cardinalidade ilimitada (IDs na URL) e sem gravar query strings (docs/14 §7). O error_log do PHP não identifica o site de forma confiável sem instrumentação.

**Decisão.**
- **Rotas:** URI normalizada (IDs → `{id}`, sem query) num Filtered Space-Saving com K = 100 por site e duas gerações de 12 h, então o relatório cobre de 12 a 24 h. Contagem com limite superior (`count`) e inferior (`count_min`), e histograma de 2 dígitos do que foi observado.
- **Erros recentes:** anéis com os últimos 50 5xx e 50 4xx do **access log** por 24 h.
- As duas estruturas ficam **só em memória** (perdem-se ao reiniciar o daemon).
- No v1, "Erros recentes" vêm dos 5xx/4xx do access log. Linhas do error_log ligadas ao site ficam para o M5 (§13).

**Alternativas.**
- Contagem exata por rota: memória sem limite com URLs de alta cardinalidade.
- Persistir rotas no SQLite: dados de baixo valor depois de 24 h e uma escrita a mais por minuto.
- Space-saving sem filtro: perde o top-20 com Zipf (medido na ADR-0048).

**Consequências.** `stats.routes` não tem janela (`RoutesDto.since` diz o que é coberto). Depois de reiniciar o daemon, a tabela recomeça vazia. Nenhuma query string chega ao disco nem à API.

### 7. Pool dedicado e liga/desliga de métricas por site
**Contexto.** O ≈ do rateio não basta para quem compara otimizações. Coletar métricas de um site custa CPU e memória, e numa raiz portátil (pendrive, pasta sincronizada) gravar `metrics_*` é indesejado.

**Decisão.** Duas colunas novas do site (M2-E5-T7), expostas em `SiteDto` (aditivo):
- **`pool_mode`** (`shared` padrão | `dedicated`). Pool dedicado = `PoolKey {versão, site}` com upstream **`php_<ver>__<slug>`** (`php_8_3__loja-test`), portas da faixa do PHP, mesma versão, blue/green e `PoolSwitcher` do pool compartilhado. Isso estende o `PoolManager` da [ADR-0029](0029-daemon-composition.md)/[ADR-0021](0021-php-pool-actor-request-estimation-blue-green.md), que hoje é por versão; o docs/06 §2.1 registra o modelo. Troca por `sites.pool_mode` (job): sobe e aquece o pool novo, re-renderiza, recarrega e drena o antigo se mais ninguém o usa.
- **`metrics_enabled`** (padrão ligado). Desligado, as linhas do site não entram no agregador e o histórico guardado fica. A troca é por `sites.metrics` (call, `operator`).
- **Raízes portáteis:** métricas **desligadas por padrão** (`[metrics] enabled` global falso quando há `portable.flag` e o usuário não definiu nada).

**Alternativas.**
- Pool dedicado sempre: dobra a memória dos pools para todo site.
- Toggle só global: não permite isolar um site barulhento.
- Guardar `pool_mode` em `prefs`: o site é a entidade, e as colunas entram na mesma migration das tabelas de métricas.

**Consequências.** Esgotar a faixa de portas do PHP vira `conflict` com mensagem clara (M2-E5-T11). O `SitePhpDto.upstream` mostra o upstream dedicado. Peers 1.0 leem `pool_mode = shared` e `metrics_enabled = true`.

### 8. Contratos da API (aditivos na 1.1)
**Contexto.** A GUI do E5 (aba Desempenho, dashboard, monitor da máquina) e a CLI precisam de dados que a 1.0 não tinha. As outras épicas do M2 consomem o `generated.ts` ao mesmo tempo, então nada pode quebrar.

**Decisão.** Tudo aditivo. Todo campo novo tem `#[serde(default)]` e o `v` dos payloads não muda. Métodos novos ficam em `NOT_IMPLEMENTED` com a tarefa dona até o backend chegar.

| Mudança | Tipo | Tarefa dona |
|---|---|---|
| `SiteStatsDto` + `from`, `bytes`, `status_1xx..5xx`, `php_p50/p90/p99_ms` (do `ut`), `workers_active`, `processes`, `last_request_at`, `metrics_enabled` | campos | M2-E5-T8 |
| `StatsParams` + `from`/`to`: merge exato de um intervalo arbitrário, alinhado para fora, limitado à retenção | campos | M2-E5-T8 |
| `stats.series {site, range, from?, to?}` → `SeriesDto {step_ms, from, to, points, markers}` | call, `viewer`, `forge stats <site> --series` | M2-E5-T9 |
| `stats.routes {site}` → `RoutesDto {since, total, routes, recent_errors}` | call, `viewer`, `forge stats <site> --routes` | M2-E5-T9 |
| `TopDto.machine` → `MachineStatsDto {cpu_pct, logical_cpus, mem_total, mem_available, forgebox_cpu_pct, forgebox_rss, disks[{role, total, free, available}]}` | campo | M2-E5-T9 (fonte: T10) |
| `SiteDto` + `pool_mode`, `metrics_enabled` | campos | M2-E5-T11 |
| `sites.pool_mode {site, mode}` → `SiteDto` | job, `operator`, mutating, `forge site pool` | M2-E5-T11 |
| `sites.metrics {site, enabled}` → `SiteDto` | call, `operator`, mutating, `forge site metrics` | M2-E5-T11 |

- **Séries:** o `range` (`StatsWindow`) escolhe o passo: `1m`/`5m` → 2 s, `1h` → 10 s, `24h` → 1 min, `7d` → 1 h (30, 150, 360, 1.440 e 168 pontos).
  - A série é **densa**: passos sem requisições vêm com contagens zero e sem percentis.
  - No máximo 2.000 pontos (`SERIES_MAX_POINTS`); acima disso é `invalid_input`.
  - Sem `from`, a série é o último `range` até `to` (padrão: agora).
- **Marcadores:** saem da tabela `events` (índice em `ts`, M2-E5-T7), sem kinds novos:
  - `site.php_changed` e `site.updated` do site;
  - `service.*` do servidor web e do pool dele;
  - `tool.updated` e `tool.rollback` do PHP dele;
  - `config.rendered` do vhost.

  Cada marcador leva `{seq, at, kind, args}`, e a GUI rotula por `kind` + `args` (como os erros da ADR-0039). **Nunca** leva o payload inteiro: o `ConfigFilePayload` tem caminho absoluto, então `config.rendered` dá só `owner` e o `fs_path` relativo à raiz.
- **CLI:** `forge stats [site] --from/--to`, `--series`, `--routes`, `forge site pool <site> shared|dedicated` e `forge site metrics <site> on|off`, cada um com `cli_template`, entrada no golden e stub no clap ([ADR-0037](0037-cli-equivalent-templates.md)). O template de `stats.get` tem duas alternativas: com `from` (intervalo exato, a janela só rotula) e sem ele.

**Alternativas.**
- Um `stats.get` com tudo, séries e rotas incluídas: respostas grandes a cada 2 s.
- Séries pelo `metrics.subscribe`: o stream não reconstrói o passado.
- Marcadores com o payload bruto do evento: vaza caminhos e acopla a GUI ao `v` de cada payload.
- Uma janela em `stats.routes`: o top-K não tem janela (§6).

**Consequências.** A GUI pode ser construída contra o fakeBridge antes do backend. Um peer 1.0 continua compatível, porque o JSON antigo lê com defaults (testes `crates/forge-ipc/tests/e5_dtos.rs`). Os nomes só congelam na tag v0.2.0.

### 9. Cadência e retomada do `metrics.subscribe`
**Contexto.** A [ADR-0015](0015-ipc-protocol.md) define `resume_from` como reenvio exato de eventos. Uma amostra de métricas é **estado** (substituída pela seguinte), não um fato histórico. Reenviar amostras velhas na reconexão só atrasaria a UI.

**Decisão.**
- **Ao assinar:** o `metrics.subscribe` responde e logo manda um **snapshot**, uma amostra por site casado, incluindo os desligados (`metrics_enabled = false`).
- **Depois:** uma amostra por site com métricas ligadas a cada **2 s** (`1m`/`5m`), **10 s** (`1h`) ou **60 s** (`24h`/`7d`) (`StatsWindow::subscribe_cadence_ms`).
- **`seq`:** é **por assinatura**, começa em `last_seq + 1`, e `last_seq` é o `resume_from` recebido (0 sem ele).
- **Retomada:** a assinatura retomada **não reenvia** o intervalo perdido. Ela manda um snapshot novo, e o buraco é preenchido pela GUI com `stats.series`.
- **Descartes da fila** (drop-oldest, 1024) saem como `metrics.lagged {missed}` no ponto em que ocorreram, como a ADR-0015 exige (todo item com `seq` > `last_seq` é entregue ou contado).

**Alternativas.**
- Replay de amostras de um anel: custo de memória por assinatura para dados que a série já tem.
- `seq` global do coletor: o `seq` de amostras de sites filtrados teria buracos que o cliente leria como perda.
- Mandar só quando muda: a janela desliza, então quase sempre muda, e o cliente perderia o relógio de atualização.

**Consequências.** O cursor de retomada do cliente (ADR-0015 item 9) funciona sem mudança: o `last_seq` devolvido nunca é menor que o cursor, então não há descarte. A GUI sabe que, depois de reconectar, deve recarregar a série visível.

### 10. Definição do orçamento de CPU
**Contexto.** A meta "< 2 % de CPU do daemon a 1.000 req/s" é ambígua: 2 % de um núcleo ou 2 % da máquina? Medida com outros builds rodando ou com a máquina quieta?

**Decisão.**
- **2 % da máquina normalizada como no Gerenciador de Tarefas**: Δcpu do processo `forgeboxd` / Δparede / CPUs lógicas, a mesma fórmula do sampler (ADR-0019 §6). O relatório informa também a **% de um núcleo** e a linha de base ociosa.
- Carga: 1.000 req/s com 64 conexões por 70 s, medindo um trecho estável de 60 s.
- Mediana de 3 execuções numa janela de **máquina quieta** agendada pelo orquestrador, com o tipo de sessão (RDP ou console) registrado.
- Gerador de carga: **`oha`**, instalado com `cargo install oha --locked --root C:/apps_src/Forgebox-wt/cache/tools` (nunca no sistema) e indicado por `FORGEBOX_OHA`.
- Percentis do `stats.get {from, to}` a ±5 % dos do `oha` (gate M2-E5-T13).

**Alternativas.**
- % de um núcleo: num i5 de 8 threads, 2 % de um núcleo seria 0,25 % da máquina, meta irreal para decodificar e agregar 1.000 linhas JSON/s.
- Medição com a máquina ocupada: não reprodutível.
- `wrk`/`ab`: sem build nativo confiável no Windows ou com percentis imprecisos.

**Consequências.** O número é comparável ao que o usuário vê no Gerenciador de Tarefas. O resultado medido entra no docs/14 ("Aceite medido") e no STATUS, com um comando para repetir.

### 11. `forge-metrics` depende só de `forge-core`
**Contexto.** O crate tem lógica pura e testável (parser, tail, HDR, atribuição). Se dependesse do supervisor, do `forge-fcgi` ou do `forge-ipc`, viraria o centro do grafo e os testes precisariam do daemon.

**Decisão.**
- `forge-metrics` usa só `forge-core` dentro do workspace, mais crates externos pequenos (`hdrhistogram`, `serde_json`, `tokio`).
- O daemon adapta as amostras do supervisor e do `forge-fcgi` para os tipos do `forge-metrics` (`PoolSample`, `AppSample`) e mapeia os resultados para DTOs do `forge-ipc`.
- A regra é verificada pelo `cargo xtask arch-guard` (regra 3, `LEAF_CRATES`) e está no grafo do [docs/01](../01-architecture.md) §3.

**Alternativas.**
- `forge-metrics` produzir DTOs: acopla o crate ao contrato de rede.
- Ler `PoolStatus` direto do `forge-fcgi`: dependência circular em potência quando o fcgi quiser métricas.

**Consequências.** Os testes do crate rodam sem processo nem rede, e mudanças no supervisor ou no fcgi não recompilam o pipeline de métricas. O mapeamento para DTO fica num lugar só do daemon (M2-E5-T8).

### 12. Nenhum DTO carrega caminho absoluto do servidor
**Contexto.** A [ADR-0009](0009-remote-ready-ipc.md) proíbe assumir que cliente e daemon estão na mesma máquina. Caminhos do daemon em DTOs vazam o layout da máquina e induzem a GUI a abri-los localmente.

**Decisão.** Os DTOs deste contrato identificam:
- discos por **papel** (`root`/`data`);
- rotas por URI normalizada;
- marcadores por `owner`/`fs_path` relativo.

Item do checklist de revisão de todo PR do E5: "nenhum DTO novo ou estendido carrega caminho absoluto do servidor". O teste `e5_dtos.rs` varre o JSON dos DTOs novos em busca de `X:\`, `X:/` e `\\`.

**Alternativas.** Mandar o caminho "informativo", como `SiteDto.root` faz: aqui não há uso para ele.

**Consequências.** A GUI remota mostra os mesmos dados que a local, e o rótulo do disco é localizado pelo papel.

### 13. Adiamentos explícitos para o M5
**Contexto.** Itens do docs/14 original custam instrumentação no PHP ou no banco, ou superfícies novas de rede, e não são necessários para responder "qual site está lento e desde quando".

**Decisão.** Vão para o **M5**:
- **toasts nativos** de alerta (no v1 os alertas de performance são internos à GUI, no dashboard, M2-E5-T17);
- o **endpoint OpenMetrics** `/_forgebox/metrics`;
- a **instrumentação profunda**: memória pico por requisição, consultas lentas correlacionadas, erros do PHP por site via `auto_prepend_file`.

No v1, "Erros recentes" vêm dos 5xx/4xx do access log (§6).

**Alternativas.**
- Entregar tudo no M2: o épico dobraria e o `auto_prepend_file` mexe no comportamento dos projetos.
- Abandonar os itens: eles ainda diferenciam o produto, então só são adiados.

**Consequências.** A UI do M2 não mostra os controles desses itens (nada de botão inerte). O docs/14 marca cada um como M5, e a exportação do v1 é `forge stats export` (CSV/JSON).

## Notas de implementação
- **M2-E5-T7.** A migration é a `0005_metrics.sql`. Os pontos de 10 s do tier 1 não são persistidos: a reidratação devolve o tier 2 (minutos e horas) e `series` passa a responder em 1 min/1 h até o tráfego novo preencher o tier 1. `metrics_tail` ganhou `closed_edge` (posição segura + borda dos minutos já gravados) para que a retomada depois de uma queda conte cada registro uma vez; detalhes em docs/14 §3.5.

- **M2-E5-T8** (serviço e integração ao daemon, [docs/14](../14-observability-metrics.md) §3.7). Decisões de implementação e trade-offs:
  - *Laço próprio com `select!` priorizado, não o `forge_core::actor::Actor`.* O ator precisa de três fontes com prioridade (mailbox > tick > lotes) para que uma consulta não espere atrás de um backlog de lotes; o `Actor` do `forge-core` só lê a mailbox (os lotes entrariam na mesma fila FIFO). Custo: sem reinício automático por pânico; o agregador não entra em pânico (fuzz do T3/T5), e um serviço parado responde `internal` em vez de travar.
  - *Lotes em pedaços de 1.024 registros.* Limita a espera de uma consulta a um pedaço (backlog de 50 MiB em debug: `stats.get` p99 entre 0,9 e 11,6 ms). A posição do lote só é anotada para a persistência quando o lote inteiro entrou.
  - *Partes antigas de um intervalo lidas fora do ator.* O ator devolve a parte em memória com os histogramas e o `MetricsHandle` lê o SQLite e mescla: o ator nunca espera o leitor do store. Alternativa rejeitada: só SQLite para `from` antigo (perderia o minuto aberto) ou só memória (7 d não cabem).
  - *A lista de sites de uma consulta vem do chamador* (`Sites::These`): o daemon é a fonte de verdade dos sites; as rotas do serviço chegam com debounce e servem ao roteamento das linhas.
  - *Partida de um arquivo sem posição guardada do byte 0 (até 64 MiB) com o checkpoint "início do arquivo" semeado.* Sem isso, linhas de um minuto ainda aberto numa parada antes do primeiro flush seriam perdidas (partida do fim) ou contadas duas vezes (partida do início sem borda). Custo: na primeira execução o histórico de até 64 MiB do arquivo é importado (o que passa de 24 h é descartado pelo agregador).
  - *`[metrics] enabled` virou opcional* (`Option<bool>`, `MetricsConfig::effective(portable)`): é o único jeito de distinguir "o usuário não definiu" de "ligado" depois que a GUI ou a CLI regravam o `forgebox.toml` inteiro. `system.config_get` devolve `null` para uma chave opcional ainda ausente e `system.config_set` passou a aceitá-la (a chave precisa existir no JSON Schema das preferências: um nome errado continua `not_found`).
  - *Fila da assinatura = a do servidor IPC (drop-oldest, 1.024).* O teste de cliente parado usa 400 sites para encher a fila em segundos; não há fila menor só para métricas.

## Checklist de revisão (PRs do E5)
- [ ] Nenhum DTO novo ou estendido carrega caminho absoluto do servidor (ADR-0009).
- [ ] Campos novos com `#[serde(default)]`; JSON 1.0 lê (teste de round trip).
- [ ] Método novo com `cli_template`, entrada no golden e stub no clap, ou `cli` vazio.
- [ ] Método sem backend em `NOT_IMPLEMENTED` com a tarefa dona; fixture da GUI espelhada.
- [ ] `cargo xtask gen-ts` sem drift; `forge-metrics` sem dependência nova do workspace.
