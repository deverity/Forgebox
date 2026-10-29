# 14 — Observabilidade: uso por projeto/vhost (CPU, memória, p50/p90/p99)

## 1. Objetivo
Uma aba **Desempenho** em cada site e um dashboard geral mostrando, por vhost e em tempo real: requisições/s, latência p50/p90/p99, taxa de erro (4xx/5xx), bytes, CPU e memória atribuídos, processos/workers, consultas lentas do banco quando possível. Sem instalar nada no projeto. O Laragon não tem nada disso; o Herd tem só "dumps".

## 2. Fontes de dados (todas já passam pelo daemon)

| Métrica | Fonte | Precisão |
|---|---|---|
| Latência por request, status, bytes, host, URI, upstream usado | **Access log estruturado** do web server, gerado pelos nossos templates: Nginx `log_format forgebox escape=json '{"t":"$msec","host":"$host","s":$status,"rt":$request_time,"ut":"$upstream_response_time","b":$body_bytes_sent,"m":"$request_method","u":"$uri","up":"$upstream_addr"}'`; Apache `LogFormat "{\"t\":\"%{msec}t\",\"host\":\"%{Host}i\",\"s\":%s,\"rt_us\":%D,\"b\":%B,\"m\":\"%m\",\"u\":\"%U\",\"up\":\"%{BALANCER_WORKER_NAME}e\"}" forgebox_json` (tempo em **µs** na chave `rt_us`, `%B` em vez de `%b` para nunca emitir `-` fora de JSON válido; o parser do daemon é por servidor); Caddy JSON log nativo. O daemon faz *tail* (polling + leitura incremental, §3.2) e agrega. | Exata, por request |
| Tempo de PHP (sem rede/servidor) | `$upstream_response_time` (Nginx) / `%{..}` do mod_proxy | Exata |
| req/s, p50/p90/p99 por janela | Agregação no daemon em **HDR histogram / t-digest** por (site, janela 10 s / 1 min / 5 min) | Exata para percentis até 3 dígitos |
| CPU/RSS por **processo** | `GetProcessTimes`/`PROCESS_MEMORY_COUNTERS_EX`/`GetProcessHandleCount` a cada 2 s, somados por Job da instância (`forge_supervisor::metrics`, [03](03-engine.md) §6; CPU % normalizado pelas CPUs lógicas, como o Gerenciador de Tarefas) | Exata por PID (±10 % de `Get-Process`, teste de aceite) |
| CPU/RSS por **vhost** | Atribuição: workers de um pool são compartilhados entre sites da mesma versão de PHP. Dois modos: **(a) `pool_mode: shared`** (padrão) → o custo do pool é rateado entre sites proporcionalmente ao `upstream_response_time` somado na janela (estimativa, marcada como "≈"); **(b) `pool_mode: dedicated`** por site → pool exclusivo, atribuição exata. Processes (`web`, `queue`) de um site são sempre exatos (PIDs próprios). | (a) estimativa, (b) exata |
| Memória pico por request PHP | Opcional: `conf.d/30-forgebox-metrics.ini` com `auto_prepend_file` que registra `memory_get_peak_usage()` e tempo em um ring buffer compartilhado (arquivo mmap em `tmp/metrics/php_<ver>.bin`) via extensão ou header `X-Forgebox-Mem` ecoado pelo servidor no log. **Opt-in** por site (toggle "instrumentação profunda"); overhead < 0,1 ms. | Exata quando ligado |
| Consultas lentas | `slow_query_log` do MySQL/MariaDB e `log_min_duration_statement` do PostgreSQL ativados por instância (toggle), tail pelo daemon, correlacionados por tempo/`FORGEBOX_SITE` (comentário SQL injetado por `auto_prepend` opcional) | Boa |
| Erros PHP | `error_log` por versão, tail, contagem por site via `FORGEBOX_SITE` no `error_log` format (`log_errors_max_len`, prefixo via `auto_prepend`) | Boa |
| Disco por projeto | Varredura periódica (1 h) do `root` + `data/` dos bancos ligados ao site | Aproximada |

## 3. Pipeline no daemon (`forge-metrics`, crate novo)
```
tail(access.log) ─▶ parse json ─▶ roteia por host→site ─▶ agrega (site, janela):
   count, errors, bytes, hdr_histogram(rt), hdr(ut)
proc sampler (2 s) ─▶ (pid→owner) ─▶ agrega por Instance/Pool/App ─▶ rateio por site (modo shared)
                                             │
                                             ▼
                      ring buffer em memória (últimas 24 h em janelas de 10 s = 8.640 pontos/site)
                                             │
            ┌────────────────────────────────┼────────────────────────────┐
            ▼                                ▼                            ▼
   SQLite `metrics_1m` (7 dias)    stream IPC `metrics.site{site, window}`   export OpenMetrics
   `metrics_1h` (90 dias)          (GUI/CLI assinam com filtro)              `/_forgebox/metrics`
```
- Percentis são calculados sobre histogramas HDR (crate `hdrhistogram`) por janela; merge de janelas é exato (não se faz média de percentis).
- Custo alvo: < 2 % de CPU do daemon a 1.000 req/s; se o log crescer mais rápido que o tail, amostragem adaptativa (marcada na UI).
- Rotação dos access logs feita pelo daemon (`logs/nginx/access.json.1`), tail segue a rotação.
- Tudo é **desligável** por site ou global (`prefs.metrics.enabled`), e desligado em `portable.flag` por padrão.

### 3.1 Parser do access log (`forge-metrics`, M2-E5-T3)
Código puro (sem I/O nem async), usado pelo pipeline de métricas e pelo alvo `site:` dos logs do daemon ([09](09-cli.md)):

- **`AccessFormat`** espelha o mapeamento `logs[].access` do manifesto ([04](04-tool-manifest-spec.md)): ponteiros JSON com unidade (`s`, `ms`, `us`, `ns`) para timestamp, host, status, duração total, duração do upstream (`multi: sum|last`), bytes, método e URI, mais `lenient_escapes`. O crate não conhece o id da tool. `ts_is_start` diz que o timestamp é o instante em que a requisição **chegou** (Apache `%{msec}t`), então `end_ts = ts + duração`; nginx (`$msec`) e Caddy (`ts`) já registram o fim.
- **`AccessParser::parse(&[u8]) -> Result<AccessRecord, ParseError>`**; `AccessRecord = {end_ts, host, status, duration_us, upstream_us?, bytes, method, route}`. O `serde_json` decodifica a linha direto para valores emprestados, guiado por uma trie dos ponteiros: só os campos mapeados são materializados e o resto (cabeçalhos, TLS do Caddy) é pulado sem alocar. Valores numéricos aceitam número ou string decimal (`"0.012"`), convertidos sem ponto flutuante para microssegundos. `ut` do nginx: `-` ou vazio vira `None`; `a, b : c` (tentativas e redirecionamento interno) vira a soma (ou o último com `multi: last`). Com `lenient_escapes`, um pré-passe reescreve o que o Apache emite e não é JSON (`\xhh`, com sequências seguidas decodificadas como UTF-8, e `\v`) e só aloca se a linha tiver uma barra invertida. Bytes que não são UTF-8 válido (o `escape=json` do nginx não valida: um `$uri` decodificado de `/%FF` grava 0xFF cru) passam antes por uma conversão com perda (U+FFFD) para todos os formatos, e só aloca nesse caso, para a requisição não sumir das métricas. Strings numéricas são validadas (só dígitos ASCII) antes de qualquer corte e a conversão usa aritmética verificada: valores absurdos viram `Invalid`, nunca pânico. Linhas com mais de **16 KiB** são rejeitadas sem decodificar; o parser nunca entra em pânico (teste de fuzz determinístico com prefixos, mutações e bytes aleatórios).
- **`HostRouter`** (snapshot em `Arc`; `rebuild` troca o snapshot e mantém o contador): host em minúsculas, sem porta e sem ponto final; primeiro host/alias exato, depois o sufixo `*.host` **mais longo** (o host de cada site vale como curinga, como no vhost de [06](06-sites-vhosts-multi-php.md) §1.3; um alias `*.x` é curinga de `x`), senão `Unrouted`, que é contado (`unrouted()`).
- **`UriNormalizer`**: remove query e fragmento (nunca armazenados, §7); segmentos só de dígitos, UUID ou hexadecimais com 16 ou mais caracteres viram `{id}`; no máximo 8 segmentos e 256 bytes; a caixa é preservada ([15](15-linux-parity-case-sensitivity.md) §7: caminhos são case-sensitive). O `request.uri` do Caddy chega percent-encoded e o `$uri` do nginx decodificado; o normalizador não decodifica.
- **Fixtures** de `crates/forge-metrics/tests/fixtures/`: linhas capturadas de nginx 1.30.6, Apache 2.4.69 e Caddy 2.11.7 reais rodando as configurações de log de `crates/forge-render/templates`. **Vazão** (teste ignorado `throughput_at_least_200k_lines_per_second`, release, uma thread, meta de 200 mil linhas/s): nginx ~1,1 M, Caddy ~0,73 M e Apache ~0,35 M linhas/s (o pré-passe de escapes custa) na máquina de desenvolvimento (i5-1135G7) com outros builds rodando; a medição oficial em máquina quieta fica para o E5-T13.

### 3.2 Tail do access log (`forge_metrics::tail`, M2-E5-T4, [ADR-0047](adr/0047-access-log-tailer-identity-checkpoints-sampling.md))
Um `Tailer` por arquivo, task própria (`tail::spawn`, ou `Tailer::new` + `run` para quem quer embrulhar o future), que entrega lotes decodificados (`TailBatch`) a um canal `mpsc` limitado do consumidor (o agregador, T8).

- **Ritmo:** tick de 250 ms ± 10 % de jitter; sem novidade o intervalo dobra até 1 s; depois de um tick que bateu o teto de leitura, o próximo vem em 50 ms. A cada tick o estado de leitura vai para `spawn_blocking` (no máximo **4 MiB por tick**, somando todos os arquivos, em leituras de 64 KiB); no runtime só roda a contabilidade entre ticks.
- **Polling, não `notify`, e só std (sem Win32):** notificações de diretório de um arquivo mantido aberto por outro processo chegam atrasadas no Windows, e olhar o caminho a cada tick custa pouco. `File::open` do std usa share mode `READ | WRITE | DELETE`: o servidor (ou o daemon, T12) renomeia e apaga o arquivo enquanto ele está em tail (teste com outro processo).
- **Linhas:** quebra em `\n`, `\r` final removido (Apache no Windows), linha parcial guardada até completar, linha em branco ignorada, linha com mais de 16 KiB (`MAX_LINE_BYTES`) contada e descartada sem guardar mais que isso. Começo no fim do arquivo (`StartAt::End`) no meio de uma linha pula até a próxima.
- **Identidade e rotação.** A cada tick, primeiro o handle aberto: encolheu (`len < offset`) ou os primeiros 256 bytes mudaram (truncado e reescrito, mesmo que já tenha passado do offset) → volta ao byte 0, descartando a linha parcial do conteúdo antigo. Depois o caminho: mesmos creation time, tamanho e LastWriteTime pelos dois handles → mesmo arquivo (o `metadata` do caminho não pede acesso de leitura, então antivírus não reescaneia o log a cada tick); senão o caminho é aberto e comparado por creation time **e primeiros 256 bytes**. O creation time sozinho não identifica o arquivo: o *tunneling* do NTFS dá a um arquivo criado com um nome removido há menos de 15 s o creation time do removido, que é exatamente o que rename + reabertura e delete + recriação fazem (medido no Windows 10 19045: os dois casos mantêm o creation time).
  - **Outro arquivo no caminho** (rename + novo, delete + recriação, rotação própria do Caddy): o handle antigo é aposentado e drenado primeiro em todo tick até ficar 2 s parado no fim (workers que ainda não reabriram); o novo é lido do byte 0. No máximo 4 aposentados ao mesmo tempo.
  - **Caminho sumiu:** o handle continua sendo lido e é solto quando drenado e parado há 5 s, ou na hora se o nome responde acesso negado (exclusão pendente na semântica antiga de delete: segurar o handle impediria recriar o nome). Nesta máquina o delete já é POSIX (o nome some na hora, mesmo com handles abertos).
  - **Arquivo ausente:** aguardado; o que aparecer depois é lido do byte 0.
- **Checkpoint** `{creation_time, offset, head_len, head_hash}`: `offset` logo depois da última linha completa entregue; `head_hash` é o FNV-1a 64 dos primeiros `head_len = min(offset, 256)` bytes (estável entre builds; nunca os bytes em si, que podem ter query string, §7). Todo `TailBatch` leva o checkpoint do arquivo no caminho, para a persistência (T7, tabela `metrics_tail`) gravá-lo na mesma transação dos dados que ele cobre; `TailStatus.delivered` é o do último lote aceito pelo canal. Retomada (`StartAt::Checkpoint`): creation time igual, `len >= offset`, hash dos primeiros bytes igual e atraso de no máximo 64 MiB; senão começa no fim com aviso (`StartOutcome::Rejected` com `CreationTime`, `Shorter`, `Content` ou `Lag`). Linhas de um arquivo aposentado ainda não drenado se perdem num reinício que caia na janela de 2 s de uma rotação.
- **Amostragem adaptativa:** backlog acima de 8 MiB, tick anterior no teto de 4 MiB ou canal cheio → `k` dobra (até 16); senão cai pela metade (até 1). Com `k > 1`, de cada bloco de `k` linhas só uma é decodificada, numa posição sorteada dentro do bloco (sem aliasing com tráfego periódico), e o lote sai com `weight = k`: contagens escaladas por ele ficam exatas a menos de um bloco parcial por troca de `k`, e o agregador marca o bucket `sampled`. Linhas são sempre contadas (`TailBatch.lines`), decodificadas ou não.
- **Saída sem bloqueio:** sempre `try_send`. Canal cheio: o lote fica pendente, nada mais é lido (o arquivo é o buffer) e o próximo tick lido amostra. O decodificador (`LineDecoder`, implementado por `AccessParser` e por closures) roda no pool de bloqueio e só nas linhas amostradas.
- **Status e ciclo de vida:** `watch` com estado (`Starting`, `Waiting`, `Following`, `Orphaned`, `Stopped`, `Failed`), resultado da partida, offset, tamanho, `k` e contadores (linhas, itens, escalado, rejeitadas, longas, bytes, rotações, truncamentos, lotes, ticks adiados). Não segurar o `Ref` do `watch` através de `.await` (é uma trava de leitura do canal). Tick em pânico → `Failed`, e o dono reinicia do `delivered`. `TailerHandle::stop(timeout)` para e devolve o status final; soltar o handle também para.
- **Medido** (`crates/forge-metrics/tests/tail.rs`, build debug, máquina com outros builds rodando): com os valores de produção, backlog de 50 MiB (373 mil linhas nginx, `AccessParser` de verdade) alcançado em ~1,5 s em 13 ticks, maior `poll` do tailer no runtime entre 0,08 e 0,7 ms em 15 execuções (orçamento: 50 ms), lotes amostrados até `k = 16` e contagem escalada a ≤ 0,001 % da real; ~86 mil linhas atravessando 6 rotações estilo nginx (rename, linhas atrasadas no arquivo antigo, reabertura) com escrita contínua chegam exatamente uma vez.

## 4. O que a UI mostra

### 4.1 Aba "Desempenho" do site
- Cabeçalho: req/s atual · p50 / p90 / p99 (últimos 5 min) · taxa de erro · CPU ≈ · RAM ≈ · workers ativos.
- Gráficos (60 s / 1 h / 24 h / 7 d): latência (bandas p50–p90–p99), throughput + erros empilhados, CPU/RAM atribuídos, memória pico por request (se instrumentação ligada).
- Tabela "Rotas mais lentas" (por `uri` normalizado: IDs numéricos → `{id}`), "Erros recentes" (linhas do error_log com link para o editor), "Consultas lentas" (se habilitado).
- Marcadores de eventos no gráfico: troca de versão de PHP, restart, mudança de config, update. Permite ver "o p99 caiu depois de habilitar OPcache".
- Botão **"Instrumentação profunda"** (toggle, explica overhead) e **"Pool dedicado"** (muda `pool_mode`).

### 4.2 Dashboard geral
- Tiles por serviço com sparklines (já em [07](07-ui-ux.md)); nova seção "Projetos" com ranking por CPU/RAM/p99/erros nas últimas 24 h; "Ociosos há > 7 dias" com sugestão de parar processes.
- Monitor da máquina: CPU/RAM/disco totais vs. parcela do Forgebox.

### 4.3 CLI
```
forge stats                           # tabela: site, rps, p50, p90, p99, err%, cpu≈, mem≈
forge stats loja.test --window 1h --json
forge stats loja.test --watch         # atualiza a cada 2 s
forge top                             # processos do Forgebox ordenados por CPU/RAM com dono (site/instance)
```

## 5. Alertas locais (opcional)
Regras simples por site: `p99 > 800 ms por 5 min`, `err% > 5 %`, `RAM do pool > 1 GB`, `disco < 2 GB`. Notificação do Windows + badge. Sem envio externo.

## 6. Exportação
- Endpoint OpenMetrics `/_forgebox/metrics` (somente 127.0.0.1 ou remoto autenticado) para quem quiser Prometheus/Grafana.
- `forge stats export loja.test --from ... --to ... -o csv|json`.

## 7. Privacidade
Métricas ficam na máquina (ou no remoto). URIs são armazenadas normalizadas; query strings nunca são gravadas; telemetria opt-in do produto nunca inclui métricas de sites.

## 8. Impacto no roadmap
- **M1**: templates já saem com `log_format` JSON e `FORGEBOX_SITE` nos params (custo zero), sampler de processos já existe.
- **M2/M3**: crate `forge-metrics` (tail + HDR + SQLite), aba Desempenho, `forge stats`, `forge top`.
- **M5**: instrumentação profunda, slow queries correlacionadas, alertas, OpenMetrics.
