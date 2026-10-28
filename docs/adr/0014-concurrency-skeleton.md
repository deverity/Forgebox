# ADR-0014 — Esqueleto de concorrência: supervisão no task do ator, travas ordenadas e writer SQLite em lotes

**Status:** aceito · **Data:** 2026-10-26

## Contexto
O [ADR-0011](0011-actor-concurrency-model.md) fixou o modelo (atores com mailbox, timeouts em toda mensagem, `catch_unwind` + reinício, `job_id` + eventos, SQLite em thread dedicada de escrita). A tarefa M0-E1-T5 implementa o esqueleto em `forge-core` e precisava decidir:
1. como reiniciar um ator em pânico sem invalidar os endereços (`ActorRef`) já distribuídos;
2. onde guardar o pouco estado compartilhado que não é de nenhum ator (tabela de jobs, registro de filhos do supervisor, pool de leitores SQLite) sem violar "nenhuma trava através de `.await`" e com ordem de travas verificável;
3. como agrupar escritas SQLite em transações curtas sem que a falha de uma operação contamine as outras;
4. como encaixar o writer no `Store` síncrono de M0-E1-T4 sem quebrar seus testes.

## Decisão
1. **Supervisão dentro do task do ator.** `ActorSupervisor::spawn(ActorSpec, factory)` cria a mailbox (`tokio::mpsc` limitada) e um task; o loop de supervisão roda nesse task e é dono do `Receiver`: `factory()` → `on_restart`/`on_start` → mensagens, tudo sob `catch_unwind`. Em pânico aplica a `RestartPolicy` (padrão: 5 reinícios em 60 s, backoff 100 ms dobrando até 5 s), publica `actor.restarted` e recria a instância **na mesma fila**: todos os `ActorRef` continuam válidos e as mensagens enfileiradas sobrevivem (a que causou o pânico se perde; o `ask` dela recebe `Stopped`). Esgotado o orçamento: status `Failed`, evento novo `actor.failed`, fila fechada (`ask`/`tell` → `Stopped`). O `ActorSupervisor` só guarda um registro (status por `watch`, token de parada, `AbortHandle`) para `children()` e `stop_all()`.
2. **`tell`/`ask` nunca esperam espaço na fila** (`try_send`): fila cheia é erro imediato (`MailboxFull`); `ask(f, timeout)` não tem versão sem timeout. Contrapressão é decisão explícita de quem envia.
3. **Parada graciosa com prazo**: `stop()` fecha a mailbox, drena o que já está nela e roda `on_stop`, tudo dentro de `stop_timeout` (padrão 5 s); passado o prazo (+250 ms) o task é abortado e o chamador recebe `Aborted` — nunca fica preso a um handler travado.
4. **Travas ordenadas (`forge_core::sync::OrderedMutex`).** O estado compartilhado que sobra usa `parking_lot::Mutex` com um `LockLevel`; só se adquire uma trava de rank estritamente maior que as já seguradas pela thread. Em debug, `debug_assert!` (thread-local) acusa violação; o guard é `!Send` e está em `await-holding-invalid-types` do `clippy.toml` (lint negado). Ordem atual: `actor.children` = `jobs.table` = `store.readers` (10, nunca aninhadas entre si) → `event_bus.seq` (200, a mais interna). A tabela fica em `forge_core::actor`.
5. **`JobRegistry` não é ator.** `start` devolve `JobId` na hora e roda o future em task próprio; `get/list/cancel` são síncronos sob a trava da tabela (microssegundos). Os eventos `job.*` são publicados **sob** essa trava (ordem `jobs.table` → `event_bus.seq`), o que garante `started < progress < done` em `seq`. Cancelamento cooperativo (`CancellationToken`); após `cancel_grace` (10 s) o future é descartado (`job.cancelled{forced: true}`); pânico vira `job.failed` com código `internal`; `request_id` repetido devolve o mesmo job (idempotência, ADR-0009); progresso limitado a um evento a cada 100 ms; retém os últimos 100 jobs terminados. Payloads versionados (`v: 1`) ficam em `forge_core::jobs::payload`.
6. **Writer SQLite.** `std::thread` dona de uma conexão própria; fila `tokio::mpsc` limitada (1024). Lote = o que **já** está na fila, até 64 operações ou 5 ms de transação (`BEGIN IMMEDIATE … COMMIT`), sem esperar por mais trabalho. Cada operação roda num `SAVEPOINT`: erro ou pânico desfaz só ela. As respostas só saem depois do `COMMIT`; se o commit falhar, todas as operações bem-sucedidas do lote recebem o erro. Operação cujo chamador já desistiu (timeout) antes de começar é pulada. As closures devolvem `forge_core::Result<T>` (e não `rusqlite::Result<T>`), para usar os repositórios (`EventRepo`) diretamente.
7. **`Store` mantém a conexão de manutenção** (`conn()`: migrações no boot, ferramentas offline, testes) e ganha `writer()` (`StoreWriter`) e `reader()` (`StoreReader`: pool de conexões `query_only` usado via `spawn_blocking`; com WAL, leitura nunca espera o writer). O `Store` é dono da thread de escrita: `close()`/drop drenam a fila e fazem `join` (no máximo 10 s); depois disso writers/leitores clonados falham com `internal`. Em memória, todas as conexões compartilham um banco `mode=memory&cache=shared` com nome único por store.
8. **Códigos de erro novos** (só acréscimos): `cancelled` e `internal` (saída 1 na CLI).

## Alternativas rejeitadas
- **Supervisor central** (um task que recebe "filho morreu" e recria): ponto único de falha e de contenção; o reinício exigiria nova mailbox ou um proxy, invalidando `ActorRef` antigos.
- **`ask` aguardando espaço (`send().await`)**: esconde sobrecarga e gasta tempo fora do controle do chamador.
- **`JobRegistry` como ator**: todo `jobs.list`/`get` viraria `ask` com timeout sem ganho real; as seções críticas são de microssegundos e a ordem de eventos fica mais simples de garantir sob a trava.
- **Uma transação por escrita**: um commit (e sync do WAL) por operação; muito mais lento com escritores concorrentes. **Lote sem savepoint**: uma operação inválida desfaria o lote inteiro.
- **Esperar até 5 ms para encher o lote**: latência artificial em toda escrita.
- **Remover `Store::conn()`**: quebraria os testes e ferramentas offline de M0-E1-T4; o risco (escrever por fora do writer) é coberto pela documentação, pela conexão ser bloqueante e `!Sync`, e pela regra de não usá-la em código async.

## Consequências
- `ActorRef` é estável durante toda a vida do ator, inclusive após pânicos.
- Um handler que bloqueia a thread (Win32 lento fora de `spawn_blocking`) prende só o próprio ator; os demais seguem em outras worker threads (`worker_threads = max(4, núcleos)`), e quem pergunta a ele recebe `Timeout`.
- Em escritas, `Timeout` significa "resultado desconhecido" se a operação já tinha começado: operações devem ser idempotentes.
- Toda trava nova precisa de um `LockLevel` e de uma linha na tabela de `forge_core::actor`.
- `tokio-console` é opt-in (feature `console` + `RUSTFLAGS="--cfg tokio_unstable"`); o build padrão não usa `tokio_unstable`.
