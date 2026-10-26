# forge-supervisor

**O que é:** Supervisor de processos (docs/03 §1 e §7): `JobTree` (Job raiz `KILL_ON_JOB_CLOSE` + Job aninhado por instância), `SpawnSpec` (PATH determinístico, env mínimo), captura de stdout/stderr (`logs`: `.out.log` com rotação 10 MiB × 5 e backpressure; broadcast drop-oldest), cascata de parada (`stop`: `stop.command` → `CTRL_BREAK` → `WM_CLOSE` → `TerminateJobObject`, eventos `service.stopping`) e um ator por instância (`InstanceActor`/`InstanceHandle`/`Supervisor`). Inclui o binário `forgebox-ctrl` (ADR-0015). Probes de saúde, políticas de restart e métricas: M1-E3-T2.

**O que não é:** Não conhece o formato de nenhuma tool (recebe `SpawnSpec`/`StopPlan` já resolvidos do manifesto) nem os pools FastCGI (isso é `forge-fcgi`). Não faz chamadas Win32: usa `forge_core::platform::{ProcessControl, ProcessGroup}`.

**Testes:** `tests/supervisor_windows.rs` usa processos reais com o serviço falso de `tests/fake_service/` (projeto cargo independente, compilado pelo teste num target próprio); `tests/php_cgi_online.rs` (`#[ignore]`, `cargo test -p forge-supervisor --test php_cgi_online -- --ignored`) baixa o PHP 8.4 NTS oficial (SHA-256 do `releases.json`) e para o `php-cgi` com `CTRL_BREAK`.

**Quem depende dele:** `forge-fcgi` e `forge-daemon`.

**Referência:** [docs/03-engine.md](../../docs/03-engine.md) e [ADR-0011](../../docs/adr/0011-actor-concurrency-model.md).
