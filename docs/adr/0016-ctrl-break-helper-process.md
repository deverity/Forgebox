# ADR-0016 — `CTRL_BREAK` entregue por processo auxiliar (`forgebox-ctrl.exe`)

**Status:** aceito · **Data:** 2026-10-26

## Contexto
A etapa 2 da parada graciosa ([03 §1.3](../03-engine.md)) é `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pgid)`. É o sinal que `php-cgi`, `node`, `mailpit` e `redis-server` tratam como "desligue". A API tem uma restrição dura: o evento só chega a processos **anexados ao mesmo console de quem chama**. Cada serviço roda com console próprio e sem janela (`CREATE_NO_WINDOW` + `CREATE_NEW_PROCESS_GROUP`), então quem envia precisa antes se anexar ao console do alvo (`FreeConsole` → `AttachConsole(pid)`).

Anexar-se a um console é estado **do processo inteiro**: um processo tem no máximo um console; o `FreeConsole` do console do próprio daemon (terminal de desenvolvimento) é irreversível; e qualquer outra thread que escreve no console (logs `tracing`, `println!`) corre contra a troca. O daemon é multi-thread por definição ([ADR-0011](0011-actor-concurrency-model.md)).

## Decisão
O daemon **nunca** troca de console. Para entregar `CTRL_BREAK` ele executa um auxiliar mínimo, `forgebox-ctrl.exe break <pid>` (bin do crate `forge-supervisor`, lógica Win32 em `forge_core::platform::windows::console_ctrl`), criado com `DETACHED_PROCESS` (sem console próprio), fora de qualquer Job do Forgebox, com stdio nulo e espera limitada (5 s; depois é morto e a cascata segue). O auxiliar:
1. instala um handler que ignora todos os eventos de console (sobrevive ao evento que gera, mesmo se o alvo não for líder de grupo);
2. `AttachConsole(pid)` no console oculto do serviço;
3. `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` (o `pid` do serviço é o id do grupo, por `CREATE_NEW_PROCESS_GROUP`);
4. `FreeConsole` e sai. O resultado volta no código de saída: `0` entregue; `1` uso; `0x1000_0000 | win32` falha no `AttachConsole`; `0x2000_0000 | win32` falha no `GenerateConsoleCtrlEvent`.

O executável fica ao lado de `forgeboxd.exe` ([08 §2](../08-release-layout.md)); o caminho é configurável (`ProcessControlOptions::ctrl_break_helper`), o que os testes usam (`CARGO_BIN_EXE_forgebox-ctrl`).

## Alternativas rejeitadas
- **`AttachConsole` temporário no próprio daemon (em `spawn_blocking`, serializado por trava global):** sem binário extra, mas o daemon perde para sempre o console em que foi iniciado (em dev, `cargo run` deixa de mostrar logs e de receber Ctrl+C), e todas as threads que escrevem em console durante a janela correm contra a troca. Uma trava global não protege escritas de bibliotecas.
- **Um console oculto compartilhado por todos os serviços (sem `CREATE_NO_WINDOW` por filho):** dispensa o anexo, mas exige que o daemon tenha um console (alocar um faz uma janela piscar no Windows 10), mistura eventos de console de todos os serviços (um `CTRL_CLOSE` derruba todos) e contraria `console: Hidden` de [03 §1.2](../03-engine.md).
- **Injetar thread remota chamando `kernel32!CtrlRoutine` no alvo (técnica do `windows-kill`):** depende de detalhe interno não documentado, exige `PROCESS_CREATE_THREAD` e é exatamente o padrão que antivírus bloqueiam.
- **PowerShell/`taskkill` como auxiliar:** PowerShell leva centenas de ms para subir e pode estar restrito por política; `taskkill` sem `/F` só envia `WM_CLOSE`, que é a etapa 3, não `CTRL_BREAK`.
- **Pular `CTRL_BREAK` e ir de `stop.command` para `TerminateJobObject`:** `php-cgi`, `node` e `redis-server` não têm comando de parada; morreriam sempre à força (sem flush, sem remover pid files).

## Consequências
- Um binário a mais na instalação (`forgebox-ctrl.exe`, sem elevação, sem rede, ~200 KB) e um processo de vida curta por parada (dezenas de ms; irrelevante perto dos timeouts de parada).
- O serviço precisa ter console (`ConsoleMode::Hidden`, padrão) e grupo próprio (`console_group`, padrão `true`); processos `Detached` não recebem `CTRL_BREAK` (o auxiliar devolve "sem console" e a cascata segue para `WM_CLOSE`/`TerminateJobObject`).
- O evento atinge o grupo inteiro no console do serviço (o serviço e os filhos que herdaram o console), como um Ctrl+Break no terminal.
- Se o auxiliar estiver ausente ou corrompido, a etapa 2 falha com erro registrado e a cascata continua: a parada nunca trava por causa dele.
- Testes reais: `php-cgi` 8.4 para em ~0,3 s via `CTRL_BREAK` sem `TerminateJobObject` (`tests/php_cgi_online.rs`); processo que ignora o evento chega a `TerminateJobObject` em `stop.timeout` (`tests/supervisor_windows.rs`).
