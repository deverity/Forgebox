# ADR-0011 — Modelo de concorrência por atores; nada bloqueia o controlador

**Status:** aceito · **Data:** 2026-10-26

## Contexto
No Laragon, uma tool que demora a parar congela a UI; no Herd, serviços que não sobem travam o app. Requisito: o daemon e a GUI respondem sempre, independentemente do estado de qualquer tool, download ou cliente lento.

## Decisão
`tokio` multi-thread + pool de bloqueio para tudo síncrono; cada Instance/PhpPool/App/subsistema é um **ator** com mailbox e estado privado; comunicação só por mensagens com timeout; nenhuma trava segurada através de `.await`; SQLite em thread dedicada de escrita; pipes de processo com backpressure para arquivo e drop-oldest para assinantes; API devolve `job_id` para operações longas e progresso por eventos; atores em pânico são isolados e reiniciados. Normativo em [03 §7](../03-engine.md).

## Alternativas rejeitadas
- **Threads do SO por serviço com `Mutex` global de estado:** contenção, deadlocks, difícil de testar.
- **Runtime single-thread (current_thread):** uma chamada Win32 lenta trava tudo.
- **Chamadas síncronas na API (esperar o start terminar):** cliente de rede/UI preso; incompatível com modo remoto.

## Consequências
- Mais código de plumbing (mensagens, timeouts), compensado por previsibilidade e testabilidade.
- Testes obrigatórios de "serviço travado", "cliente lento" e "download pesado" ([03 §7.6](../03-engine.md)).
- `tokio-console` em dev para flagrar bloqueios > 1 ms.
