# ADR-0048 — Agregador de métricas: HDR esparso, minutos com histograma por orçamento sobre horas exatas, rotas por Filtered Space-Saving

**Status:** aceito · **Data:** 2026-10-29 · Implementa o agregador do desenho de métricas v1 ([docs/14](../14-observability-metrics.md) §3.4; a ADR das métricas v1, M2-E5-T1, o referencia). Segue a [ADR-0011](0011-actor-concurrency-model.md): o agregador é estado de um único dono (o ator de métricas do M2-E5-T8), síncrono e sem I/O.

## Contexto
O plano do M2-E5 pediu, por site: buckets de 2 s com histogramas HDR de rt e ut por 10 min, pontos escalares de 10 s (~64 B) por 24 h, histogramas HDR compactos de 1 min por 24 h, top-K (K = 100) de rotas com HDR de 2 dígitos e os últimos 50 5xx e 50 4xx; HDR com 3 dígitos significativos de 1 µs a 10 min; merges exatos (nunca média de percentis); e orçamentos de memória: site ativo a 1.000 req/s por 24 h ≤ 2 MiB, ocioso ≤ 16 KiB, 50 sites mistos ≤ 40 MiB, ≤ 1 µs por registro, top-20 verdadeiro sempre presente com Zipf.

As contas não fecham com o desenho literal:
- Um `Histogram<u32>` denso do `hdrhistogram` nesse layout tem 21.504 contadores (84 KiB). Os 300 buckets de 2 s × 2 histogramas dariam 50 MiB por site.
- Mesmo esparso, um minuto a 1.000 req/s com latência log-normal (mediana 20 ms, p99 ~120 ms, o gate do E5-T13) tem ~350 valores distintos quando o servidor registra em milissegundos (nginx `$request_time`) e ~8.000 quando registra em microssegundos. Isso custa ~1,5 KB (rt + ut) por minuto em ms, ou ~40 KB em µs. 1.440 minutos dariam 2,2 MB ou 58 MB, acima dos 2 MiB mesmo no caso favorável. Pela entropia das contagens, nenhuma codificação sem perda resolve.
- Medido nas fixtures reais desta máquina: o nginx registra em ms, o Apache no tick do relógio (0 ou 15.638 µs) e o Caddy em resolução abaixo de 1 ms (0,0003858 s).
- Space-saving puro com K = 100 sobre Zipf(1) de 10 mil rotas não garante o top-20. A frequência da 20ª rota (0,51 % de N) é menor que N/K, e o menor contador sobe no ritmo da cauda (47 % do tráfego fora do top-100). Rotas a partir da ~13ª posição seriam despejadas sem parar.

## Decisão
1. **Layout HDR do crate, armazenamento próprio.** `forge_metrics::hist` reproduz a aritmética de índices do `hdrhistogram` 7 (mínimo 1 µs, máximo 10 min com valores maiores limitados, 3 dígitos; rotas com 2) e testa que índices, equivalentes, quantis e `iter_recorded` são idênticos aos do crate. Cada histograma é guardado como a lista esparsa dos índices não vazios em varints (`gap << 1 | (count == 1)`, mais `count - 2`). O crate (`default-features = false`, só `serialization`) dá a codificação V2 dos blobs persistidos (`FORMAT_VERSION = 1`) e é o oráculo dos testes. Merges somam índice a índice num histograma denso de rascunho (168 KiB, um por agregador, faixa tocada rastreada).
2. **Buckets abertos como amostras cruas.** Enquanto aceita registros, um bucket de 2 s guarda `índice << 16 | peso` (4 B por registro, buffers reaproveitados entre sites). Ao fechar, as amostras são ordenadas e compactadas. Gravar um registro custa um índice, um `push` e as contagens.
3. **Tempo pelo `end_ts`, relógio monotônico.** O relógio é o maior `now` recebido em `advance`, e um `now` menor é ignorado e contado. Registro do futuro é limitado ao relógio. O bucket de 2 s aceita registros até 2 s depois do fim; depois disso o registro é atrasado e vai **só para o tier 2** (acumulador por minuto, mesclado no fechamento ou no minuto e na hora já fechados), de modo que tier 0 e tier 1 nunca mudam depois de mostrados. Mais velho que a retenção: descartado e contado.
4. **Tier 2 = minutos sobre horas.** Todo minuto fechado guarda contagens e p50/p90/p99 de rt e ut por 24 h, e é mesclado na sua hora, que guarda histogramas rt/ut por 24 h. O histograma do próprio minuto fica pelo menos `minute_hist_keep` (1 h) e depois enquanto o site couber em `minute_hist_budget` (128 KiB), os mais antigos saindo primeiro. Acima de `minute_hist_cap` (1 MiB) saem mesmo dentro da hora, o que só acontece com durações em µs a centenas de req/s. Os histogramas de 1 min por 24 h do plano viram "1 min onde couber, 1 h sempre": as horas mantêm a janela de 24 h exata e as séries de 24 h a 60 s usam os percentis guardados por minuto.
5. **Planejador de intervalos.** `range`/`window` montam a resposta com buckets inteiros alinhados para fora, do tier mais fino disponível:
   - horas fechadas inteiras pela hora;
   - minutos fechados inteiros pelo minuto, ou pela hora se o histograma do minuto saiu;
   - pontas parciais pelo tier 0 enquanto ele tem o trecho, senão pelo minuto inteiro;
   - minutos abertos pelo tier 0, mais os atrasados quando o minuto inteiro está no intervalo.

   O trecho coberto volta em `Summary.from`/`to`.

   O planejador nunca começa antes do **horizonte** (início da hora que contém `now - retenção`) nem passa do relógio. `from`/`to` e `span` vêm do cliente (`stats.get`, `stats.series`), então o custo de uma consulta é limitado pela retenção (no máximo 7 d, `MAX_RETENTION`), não pelo intervalo pedido: `range(Timestamp::MIN, now)` e `window(Duration::MAX)` custam o mesmo que uma janela de 24 h.
6. **Rotas por Filtered Space-Saving** (Homem & Carvalho, 2010). Cada site tem K = 100 entradas monitoradas e um filtro de 1.024 contadores indexados pelo hash (`RandomState`, resistente a HashDoS) da rota. Uma rota de fora só entra quando a sua célula mais o peso alcança o menor contador. A entrada despejada deixa a contagem na sua célula, então contagem é limite superior e `count - overcount` o inferior. O histograma de 2 dígitos e os erros cobrem só o observado. São duas gerações de 12 h, e o relatório mescla as duas.
7. **Erros recentes** em dois anéis (50 5xx, 50 4xx) com rota normalizada, sem query string, podados após 24 h.
8. **Ganchos de persistência.**
   - `minute_outbox` entrega, por `take_minutes`, o minuto completo ao fechar e os atrasados de minutos já fechados como deltas a somar. A fila é limitada a 8.192 itens, com descarte contado.
   - `restore_minute` reconstrói minuto e hora no boot. Um minuto ainda aberto entra como atrasado.
   - O agregador não conhece SQLite.
9. **Medido como aceite.** Um alocador global que conta por thread mede memória real. A estimativa por capacidade (`site_heap_bytes`) fica a 0,1 % do medido e serve para o daemon expor o próprio custo. Os testes de 24 h e o de custo por registro rodam em release e são ignorados.

## Alternativas rejeitadas
- **`hdrhistogram::Histogram` em cada bucket** (com ou sem `auto_resize`): array denso de 84–168 KiB por histograma; 50 MiB por site no tier 0.
- **V2 + DEFLATE em memória:** ~1,5–2× menor que os varints em ms, mas descomprimir a cada consulta e mesmo assim acima de 2 MiB com 1.440 minutos × 2 histogramas.
- **Histogramas de 1 min por 24 h com 2 dígitos:** com durações em ms o número de índices distintos não cai abaixo de ~256 ms (cada ms já é um índice), então não economiza; e a janela de 24 h perderia precisão.
- **t-digest / quantis aproximados:** merges não exatos; o docs/14 exige percentis de histogramas mesclados.
- **Reabrir buckets fechados para atrasados:** um ponto de 2 s ou 10 s já enviado ao cliente mudaria depois; os atrasados (rotação drenada, retomada de checkpoint) ficam nos minutos e horas, que são os que se persistem.
- **Relógio que acompanha saltos para trás:** exigiria inserir buckets antes de outros já fechados e invalidar pontos/minutos "do futuro". Monotônico é simples e nunca conta duas vezes; o custo é o tier 0 parado até o relógio alcançar o valor antigo (o T8 pode recriar o agregador num salto grande).
- **Space-saving puro, ou K maior:** puro perde o top-20 com Zipf(1); K = 400 com histogramas quadruplicaria a memória de rotas, enquanto o filtro custa 4 KiB.
- **Uma geração de rotas com decaimento exponencial:** contagens deixam de ser contagens de um período definido.

## Consequências
- Site ativo a 1.000 req/s por 24 h: **1,52 MiB** com durações em ms (nginx, Apache no Windows) e **5,1 MiB** com durações em µs (Caddy no Windows), dos quais ~1,6 MB são do tier 0 e ~1,35 MB das horas, inevitáveis a 3 dígitos com um valor distinto por requisição. O orçamento de 2 MiB vale para logs em ms; o caso em µs é medido e tem limite de regressão (6 MiB). 50 sites mistos: 36,5 MiB. Ocioso: ~1 KiB.
- Para sites muito ativos, intervalos com minutos mais velhos que ~1 h se alinham à hora em memória. O T7 lê `metrics_1m` do SQLite quando precisar de alinhamento por minuto nesse trecho.
- Pontas parciais lidas do tier 0 não incluem os atrasados daquele trecho; minutos e horas incluem.
- Depois de um salto para trás do relógio do sistema, janelas de 1m/5m e pontos de 10 s ficam parados até o relógio passar do valor antigo; contagens continuam nos minutos.
- O formato persistido é o V2 do `hdrhistogram`, interoperável com outras ferramentas HDR; mudar o layout exige outro `FORMAT_VERSION`.
- Custo: 212 ns por registro em release, com um `advance` a cada 250 registros.
- CPU/RAM por site usa o tipo da atribuição (`forge_metrics::SiteResources`, M2-E5-T6, [docs/14](../14-observability-metrics.md) §3.3) na API (`set_resources`, `SeriesPoint.resources`), sem tipo paralelo; o ponto de 10 s de 64 B o guarda empacotado (CPU em `f32`, memória em KiB, workers em `u16`, com conversão saturada). `Counters::site_load()` dá o `SiteLoad` de entrada da atribuição a partir de `sums`.
- Tudo é parametrizável em `AggConfig` (graça, spans, orçamento e teto dos minutos, K, época das rotas, anéis, fila).
