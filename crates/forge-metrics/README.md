# forge-metrics

**O que é:** Tail de access logs JSON `forgebox`, histogramas por site e janela, rateio de CPU/RAM por vhost e tabelas `metrics_*`.

**O que não é:** Não expõe API: os dados chegam ao cliente pelo `forge-ipc`, via daemon.

**Quem depende dele:** `forge-daemon`.

**Referência:** [docs/14-observability-metrics.md](../../docs/14-observability-metrics.md).

**Estado:** o parser de registro de acesso (`AccessFormat`, `AccessParser`, `AccessRecord`), o roteador host->site (`HostRouter`) e o normalizador de URI (`UriNormalizer`) estão prontos (M2-E5-T3, [docs/14 §3.1](../../docs/14-observability-metrics.md)); o tailer de logs (`tail`: polling, linhas parciais, rotação com identidade por creation time + primeiros bytes, checkpoints, amostragem adaptativa) também (M2-E5-T4, [docs/14 §3.2](../../docs/14-observability-metrics.md)); o agregador (`Aggregator`: camadas de 2 s, 10 s, minutos e horas com histogramas HDR esparsos e merges exatos, top-K de rotas, erros recentes, deltas de minuto para a persistência) também (M2-E5-T5, [docs/14 §3.4](../../docs/14-observability-metrics.md), [ADR-0048](../../docs/adr/0048-metrics-aggregator-sparse-hdr-tiers-routes.md)); a atribuição de CPU/RAM por vhost (`attribute` → `SiteResources`, gravada nos pontos de 10 s com `Aggregator::set_resources`) também (M2-E5-T6, [docs/14 §3.3](../../docs/14-observability-metrics.md)); persistência e o ator vêm nas tarefas seguintes do E5.
