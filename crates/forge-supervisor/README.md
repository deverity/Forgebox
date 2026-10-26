# forge-supervisor

**O que é:** Spawn de processos com Job Objects, captura de stdout/stderr, probes de saúde, políticas de restart e parada gradual.

**O que não é:** Não conhece o formato de nenhuma tool nem os pools FastCGI (isso é `forge-fcgi`).

**Quem depende dele:** `forge-fcgi` e `forge-daemon`.

**Referência:** [docs/03-engine.md](../../docs/03-engine.md) e [ADR-0011](../../docs/adr/0011-actor-concurrency-model.md).
