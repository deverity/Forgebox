# 14 — Observabilidade: uso por projeto/vhost (CPU, memória, p50/p90/p99)

## 1. Objetivo
Uma aba **Desempenho** em cada site e um dashboard geral mostrando, por vhost e em tempo real: requisições/s, latência p50/p90/p99, taxa de erro (4xx/5xx), bytes, CPU e memória atribuídos, processos/workers, consultas lentas do banco quando possível. Sem instalar nada no projeto. O Laragon não tem nada disso; o Herd tem só "dumps".

## 2. Fontes de dados (todas já passam pelo daemon)

| Métrica | Fonte | Precisão |
|---|---|---|
| Latência por request, status, bytes, host, URI, upstream usado | **Access log estruturado** do web server, gerado pelos nossos templates: Nginx `log_format forgebox escape=json '{"t":"$msec","host":"$host","s":$status,"rt":$request_time,"ut":"$upstream_response_time","b":$body_bytes_sent,"m":"$request_method","u":"$uri","up":"$upstream_addr"}'`; Apache `LogFormat "{\"t\":\"%{msec}t\",\"host\":\"%{Host}i\",\"s\":%s,\"rt_us\":%D,\"b\":%B,\"m\":\"%m\",\"u\":\"%U\",\"up\":\"%{BALANCER_WORKER_NAME}e\"}" forgebox_json` (tempo em **µs** na chave `rt_us`, `%B` em vez de `%b` para nunca emitir `-` fora de JSON válido; o parser do daemon é por servidor); Caddy JSON log nativo. O daemon faz *tail* (notify + leitura incremental) e agrega. | Exata, por request |
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
- **`AccessParser::parse(&[u8]) -> Result<AccessRecord, ParseError>`**; `AccessRecord = {end_ts, host, status, duration_us, upstream_us?, bytes, method, route}`. O `serde_json` decodifica a linha direto para valores emprestados, guiado por uma trie dos ponteiros: só os campos mapeados são materializados e o resto (cabeçalhos, TLS do Caddy) é pulado sem alocar. Valores numéricos aceitam número ou string decimal (`"0.012"`), convertidos sem ponto flutuante para microssegundos. `ut` do nginx: `-` ou vazio vira `None`; `a, b : c` (tentativas e redirecionamento interno) vira a soma (ou o último com `multi: last`). Com `lenient_escapes`, um pré-passe reescreve o que o Apache emite e não é JSON (`\xhh`, com sequências seguidas decodificadas como UTF-8, e `\v`) e só aloca se a linha tiver uma barra invertida. Linhas com mais de **16 KiB** são rejeitadas sem decodificar; o parser nunca entra em pânico (teste de fuzz determinístico com prefixos, mutações e bytes aleatórios).
- **`HostRouter`** (snapshot em `Arc`; `rebuild` troca o snapshot e mantém o contador): host em minúsculas, sem porta e sem ponto final; primeiro host/alias exato, depois o sufixo `*.host` **mais longo** (o host de cada site vale como curinga, como no vhost de [06](06-sites-vhosts-multi-php.md) §1.3; um alias `*.x` é curinga de `x`), senão `Unrouted`, que é contado (`unrouted()`).
- **`UriNormalizer`**: remove query e fragmento (nunca armazenados, §7); segmentos só de dígitos, UUID ou hexadecimais com 16 ou mais caracteres viram `{id}`; no máximo 8 segmentos e 256 bytes; a caixa é preservada ([15](15-linux-parity-case-sensitivity.md) §7: caminhos são case-sensitive). O `request.uri` do Caddy chega percent-encoded e o `$uri` do nginx decodificado; o normalizador não decodifica.
- **Fixtures** de `crates/forge-metrics/tests/fixtures/`: linhas capturadas de nginx 1.30.6, Apache 2.4.69 e Caddy 2.11.7 reais rodando as configurações de log de `crates/forge-render/templates`. **Vazão** (teste ignorado `throughput_at_least_200k_lines_per_second`, release, uma thread, meta de 200 mil linhas/s): nginx ~1,1 M, Caddy ~0,73 M e Apache ~0,35 M linhas/s (o pré-passe de escapes custa) na máquina de desenvolvimento (i5-1135G7) com outros builds rodando; a medição oficial em máquina quieta fica para o E5-T13.

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
