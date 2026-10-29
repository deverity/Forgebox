# ADR-0019 — Ciclo de vida da Instance: máquina de estados pura, health, políticas de restart e métricas por Job

**Status:** aceito · **Data:** 2026-10-27

## Contexto
[03 §1.4](../03-engine.md) define os estados de uma Instance (`Stopped`, `Starting`, `Running`, `Stopping`, `Crashed(code)`, `Failed(reason)`, `Maintenance`), probes de health, políticas de restart (`never`, `on-failure{max, window, backoff}`, `always`) e métricas de processo a cada 2 s. O ator da instância (M1-E3-T1) já spawna, observa a saída e para em cascata sem nunca esperar ([ADR-0011](0011-actor-concurrency-model.md)). Faltava decidir: onde moram as regras de transição, o que acontece com um serviço que fica doente depois de pronto, como o restart se comporta em loop, e como medir CPU/memória de um serviço com vários processos de forma comparável ao Gerenciador de Tarefas.

## Decisão
1. **Máquina de estados pura.** `forge_supervisor::service::state::next_state(&estado, gatilho) -> Result<estado, InvalidTransition>` é o único lugar que decide transições. Gatilhos: `Start`, `Healthy`, `Exited{code}`, `Stop{cause}`, `StopFinished{code}`, `Fail{reason}`, `EnterMaintenance`, `ExitMaintenance`. O ator traduz mensagens e avisos das suas tasks em gatilhos; transição inválida vira `ForgeError::Conflict`, nunca pânico. Teste exaustivo estado × gatilho (11 × 10).
2. **`Stopping` carrega a causa** (`user`, `restart`, `maintenance`, `unhealthy`, `start_timeout`). A mesma cascata de parada (§1.3) serve a todas; o destino depende da causa: `Stopped`, `Crashed` (health falhou com o serviço já `Running`, para a política decidir) ou `Failed(HealthTimeout)` (prazo de start). "Qualquer estado → `Maintenance`" é parar com `cause: maintenance` e então entrar.
3. **`Failed` estruturado** (`FailureReason`, tag `kind`): `port_in_use{port, pid, exe}`, `missing_dll{exit_code}` (`0xC0000135`/`0xC0000139`), `config_invalid{stderr}`, `permission{message}` (inclui saída `0xC0000022`), `health_timeout{timeout_ms, last_error}`, `exited_during_start{exit_code}`, `crash_loop{restarts, window_ms, exit_code}`, `other{code, message}`. Cada variante tem um código estável de `ForgeError` para `error.code`; a UI escolhe a ação por `kind`. Mesma forma em `forge_ipc::api::services::FailureReasonDto` (teste de contrato).
4. **Health** em um task por execução: primeiro probe logo após o spawn, depois `start_interval` (500 ms) até passar uma vez e `interval` (5 s) depois, sempre ±10 % de jitter; cada probe com timeout próprio. O probe HTTP não usa proxy, não segue redirect e **não verifica certificado** (só lê o status). O probe `command` roda num Job filho da raiz e é morto ao fim do probe. `failure_threshold` falhas seguidas em `Running` param a instância (`cause: unhealthy`) — exceto com política `never`, em que ela segue `Running` e o `Status` mostra o health falhando.
5. **Política de restart** em `RestartTracker` (puro, tempo injetado): janela deslizante; espera `min · 2^n` limitada a `max`, com `n` = reinícios automáticos dentro da janela (para `always`, janela fixa de 60 s). `on-failure` não reinicia saída 0. Saídas que nenhum restart resolve (DLL faltando, acesso negado) vão para `Failed` sob qualquer política. Start/restart manual zera o histórico; `Stop` cancela o restart agendado.
6. **Métricas por Job, CPU normalizada pela máquina.** O sampler lista os PIDs do Job da instância e lê `GetProcessTimes`, `GetProcessMemoryInfo` (`PROCESS_MEMORY_COUNTERS_EX`) e `GetProcessHandleCount` por PID, atrás de `forge_core::platform::ProcessStats`, em `spawn_blocking`. CPU % = Δcpu / Δparede / CPUs lógicas (todas as processor groups), igual à coluna CPU do Gerenciador de Tarefas; processos identificados por `(pid, criação)`. A última amostra fica num `watch` (lido por `ask(Metrics)` e assinado por `SubscribeMetrics`); o evento `service.metrics` é opt-in por instância.

## Alternativas rejeitadas
- **Transições espalhadas nos handlers do ator (como em M1-E3-T1):** cada handler decidia o próximo estado; impossível testar a tabela inteira sem processos reais e fácil aceitar combinações inválidas.
- **Health falhando em `Running` → `Failed` direto:** perderia o restart automático, que é justamente o que resolve um serviço travado. **→ estado novo `Unhealthy`:** a UI ganharia um estado sem ação própria; o `Status.health` já mostra "rodando, mas falhando".
- **Backoff contado desde a criação da instância:** um serviço estável por horas voltaria a esperar 30 s na primeira queda. Contar na janela recomeça em `min`.
- **Reiniciar também `missing_dll`/acesso negado:** gastaria `max` tentativas para chegar ao mesmo diagnóstico e atrasaria a oferta de instalar o VC++ Redistributable.
- **PDH (`\Process(*)\% Processor Time`):** contadores por nome de imagem (`php-cgi#3`), que mudam quando processos saem; consulta mais cara e localizada. `GetProcessTimes` por PID é exato e barato.
- **CPU % por núcleo (estilo `top`, até N × 100 %):** não bate com o Gerenciador de Tarefas, que é a referência do usuário Windows; o valor por núcleo é derivável (× CPUs lógicas).
- **Verificar certificado no probe HTTP:** serviços locais usam a CA do Forgebox (talvez ainda não confiada) ou certificados autoassinados; o probe não envia nem lê dados além do status.
- **`service.metrics` sempre ligado:** 1 evento a cada 2 s por instância seria persistido no SQLite (§8) sem ninguém assistindo; o stream IPC assina o `watch` sob demanda.

## Consequências
- Novos kinds `service.maintenance` e `service.metrics`; `ServicePayload` ganhou `cause`, `restart_in_ms` e `reason` (aditivos, `v` = 1).
- O mapeamento manifesto → `HealthSpec`/`RestartPolicy` (inclusive `start_interval`, `timeout` e `backoff`, que não estão no manifesto e usam padrões) fica para M1-E5-T2; `port_in_use`/`config_invalid` são produzidos pelas pré-checagens (M1-E3-T3).
- A CPU de um processo que nasce e morre entre duas amostras não é vista; processos vistos pela primeira vez contam o que usaram dentro da janela.
- A redução de frequência do sampler sob carga (§7.4) ainda não existe: o intervalo é fixo por instância.

## Adendo M2-E5-T10 — CPU/RAM/disco da máquina e nomes de imagem (2026-10-29)

**Contexto.** O monitor da máquina do dashboard e o `forge top` ([14](../14-observability-metrics.md) §4.2-§4.3) mostram CPU/RAM/disco totais ao lado da parcela do Forgebox, e cada PID com o nome do executável. O §6 acima só mede processos.

**Decisão** (`forge_core::platform::{system_stats, process_stats}`, Win32 em `platform/windows/{system_stats,process_stats}.rs`):
1. **`SystemStats`** (`sample() -> MachineSample`, `volume(path) -> VolumeSpace`), bloqueante como `ProcessStats`, com `sample_machine`/`volume_space` (`spawn_blocking`, prazo de 5 s), `FakeSystemStats` e `UnsupportedSystemStats`; injetável por `Platform::with_system_stats`.
2. **CPU da máquina por `GetSystemTimes`**, contadores acumulados somados em todos os processadores lógicos (todas as processor groups): ocupado = kernel − ocioso + usuário (o kernel inclui o ocioso). CPU % entre duas amostras = Δocupado / (Δocupado + Δocioso), **sem relógio de parede**. A parcela de processos usa a **mesma capacidade** (`cpu_share_since(Σ Δcpu_total)`), limitada a 100.
3. **Memória por `GlobalMemoryStatusEx`** (`ullTotalPhys` = `Win32_ComputerSystem.TotalPhysicalMemory`; `ullAvailPhys` = "Disponível"); **disco por `GetDiskFreeSpaceExW`** com separador final (UNC exige), um arquivo respondendo pelo volume da pasta dele, `total`/`free`/`available` (cota do usuário).
4. **Nome de imagem por `ProcessStats::info(pid)`**: um handle `PROCESS_QUERY_LIMITED_INFORMATION` dá a criação e `QueryFullProcessImageNameW` (o mesmo auxiliar da tabela TCP), então o nome é do processo daquela identidade. Método com implementação padrão (sem imagem): os fakes existentes continuam valendo.
5. **`ImageNameCache`** separado, do dono do sampler: chave `(pid, started_at)`; PID reutilizado responde `None` para a identidade antiga e o novo processo fica lembrado com a sua; processo que sumiu não é lembrado; acesso negado é lembrado como sem nome; `retain` por rodada e recomeço acima de 4.096 entradas; trava `platform.image_names` (rank 10, folha, nunca durante a consulta ao SO).

**Alternativas rejeitadas.**
- **PDH `\Processor(_Total)\% Processor Time` ou `% Processor Utility`:** precisa de consulta aberta e duas coletas, nomes de contador localizados e custo maior; a "Utilidade" (escalada por frequência, a do Gerenciador de Tarefas com turbo) não é comparável à CPU por processo do §6, que é por tempo. `GetSystemTimes` é uma chamada barata (11–90 µs medidos) e mede a mesma grandeza que `GetProcessTimes`.
- **CPU % = Δocupado / (Δparede × CPUs):** depende do instante exato das duas leituras (o sampler tem jitter) e passa do real depois de uma suspensão/hibernação, em que o relógio anda e os contadores não. A razão ocupado/(ocupado + ocioso) não tem esses erros; a parcela dos processos no mesmo denominador garante "Forgebox ≤ máquina".
- **`NtQuerySystemInformation(SystemProcessorPerformanceInformation)`:** por processador, API não documentada como estável; o total basta.
- **`image_name` em `ProcStats`:** cada amostra (2 s por PID) consultaria ou clonaria o nome e todos os construtores de `ProcStats` (a impl Windows e os fakes de forge-core, forge-ipc e forge-supervisor) quebrariam; o nome muda só quando a identidade muda.
- **Cache global dentro de `WindowsProcessStats`:** estado escondido sem dono para podar, e fakes não o exercitariam. **Cache só por PID:** PID reutilizado mostraria o nome do processo anterior.
- **Lembrar `not_found`:** a identidade não volta; só cresceria a tabela.

**Consequências.**
- O daemon (M2-E5-T9) compõe `TopDto.machine` e a parcela do Forgebox a partir de `Platform::system_stats()` e das amostras de processo que já coleta; nada de IPC nesta tarefa.
- O número é `% Processor Time`: pode ficar abaixo do gráfico do Gerenciador de Tarefas quando o processador está em turbo (lá é "Utilidade"). Documentado em [03](../03-engine.md) §6.
- Aceite medido nesta máquina (Win10 19045, i5-1135G7 com 8 CPUs lógicas, builds de outros worktrees rodando): `mem_total` exato; `mem_avail` e `volume` dentro do intervalo das nossas leituras antes/depois do PowerShell (±10 % / ±1 %); serviço falso a 50 % de uma CPU → máquina em 2 s ≥ 6,25 % e ≥ a parcela medida do serviço; nome da imagem do processo de teste = `current_exe`.
