# ADR-0045 — Extras de segurança: desinstalar a CA, encerrar quem ocupa uma porta, `.env` mascarado

**Status:** aceito · **Data:** 2026-11-03 · Implementa os três métodos que a [ADR-0040](0040-e3-api-terminal-db-mail-logs.md) definiu (`tls.ca_uninstall`, `services.kill_port_holder`, `sites.env`; M2-E3-T11). Segue a [ADR-0022](0022-elevated-helper-protocol.md) (protocolo do helper), a [ADR-0023](0023-local-ca-name-constraints-trust.md) (CA e livro de confiança), a [ADR-0024](0024-helper-execution.md) (execução no helper), a [ADR-0009](0009-remote-ready-ipc.md) (`local_only`, papéis) e a [ADR-0011](0011-actor-concurrency-model.md) (I/O bloqueante em `spawn_blocking`).

## Contexto
A 0.1.0 saiu com três lacunas de segurança/UX que o docs/07 (§1.2 "Encerrar | Trocar porta", §5.4 aba Ambiente) e a auditoria do M2 pediram:
1. **Não havia como tirar a CA do Forgebox dos stores** pela API (problema conhecido da 0.1.0). O `TlsManager::uninstall_ca` já existia (remove a CA atual e as antigas do livro `certs/ca/trust.json`), mas nenhum método o chamava.
2. **"Encerrar" de um conflito de porta** exige o daemon matar um pid arbitrário — a ação mais perigosa que ele faz sem o helper.
3. **O visualizador de `.env`** precisa ler arquivos fora de `www/` (sites ligados) sem abrir a API para caminhos arbitrários e sem deixar segredos saírem para clientes remotos.

## Decisão
### 1. `tls.ca_uninstall {scope}` (job, `admin`)
- O job chama `TlsManager::uninstall_ca(scope)`: a CA atual **e** toda raiz registrada no livro para aquele escopo; publica `tls.ca_untrusted {scope, thumbprint_sha1}` por raiz removida e devolve o `CaStatusDto`. Os arquivos da CA ficam (a emissão continua; os sites só deixam de ser confiáveis até um novo `tls.ca_install`). Idempotente: nada no store = nada perguntado, nenhum evento, o mesmo status. Modo portátil: `forbidden` (nunca toca no store).
- `current_user`: o diálogo de confirmação do Windows, uma vez por raiz presente (recusado → `cancelled`).
- `local_machine`: pelo `forgebox-helper` com a op **existente** `trust.remove_machine_root {thumbprint}`. Em vez de uma op nova, a validação dela foi endurecida: o certificado encontrado em `LocalMachine\Root` precisa passar **no mesmo teste da instalação** (`check_machine_root_candidate`: autoassinado, `cA`, CN começando com `Forgebox`, Name Constraints só com sufixos de uso local e IPs loopback/privados). Antes bastava o CN começar com `Forgebox`.
- Testes sem tocar nos stores reais: `BootOptions::trust_store` recebe uma fábrica que ganha o `HelperClient` do daemon; os testes usam `forge_core::platform::MemoryTrustStore`, cujas mudanças em `LocalMachine` passam pelo `HelperClient` (com a `FakeElevation` injetada via `Platform::with_elevation`, agora usada pelo boot também para o helper).

### 2. `services.kill_port_holder {port, pid, confirm}` (`admin`, `local_only`, `mutating`)
- `confirm` tem de ser `true` (`invalid_input`); remoto → `forbidden` pela tabela (`local_only`).
- Decisão em duas partes, ambas em `forge_core::platform::process_kill`:
  - **`KillPolicy::check`** (pura, testada com fatos fabricados): recusa (`forbidden`) PID 0/4, o próprio daemon, pids da árvore de Jobs do daemon (serviços, pools, terminais), executáveis dentro da raiz do Forgebox, os programas do Forgebox por nome (`forgeboxd`, `Forgebox`, `forge`, `forgebox-helper`, `forgebox-ctrl`), processos críticos (`IsProcessCritical`), processos que hospedam serviços (`EnumServicesStatusExW`, inclusive serviços por usuário), sessão 0, contas de sistema (`S-1-5-18/19/20`), processos de outro usuário ou cujo token não se lê, executáveis sob `%SystemRoot%` e processos sem caminho de imagem.
  - **`ProcessTerminator::terminate_checked`** (`platform/windows/process_kill.rs`): abre o pid **uma vez** (`PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE`), lê os fatos desse handle, roda a decisão do chamador — a política **mais** "este pid ainda escuta nesta porta" (tabela TCP relida com o handle aberto; senão `conflict`) — e chama `TerminateProcess` + `WaitForSingleObject` (até 5 s) **no mesmo handle**. Um handle aberto mantém o objeto do processo vivo, então o pid não pode ser reciclado entre a checagem e o kill.
- Tudo bloqueante numa só `spawn_blocking_timeout` (10 s; orçamento do método 12 s).
- Auditoria: evento `port.holder_killed {port, pid, exe, client}` e linha `warn` no log do daemon ("audit: port holder terminated"); recusas também vão ao log com o motivo.

### 3. `sites.env {site, reveal?}` (`operator`; `reveal` exige `admin`)
- O pedido nomeia um **site**, nunca um caminho. O daemon lê só `.env` e `.env.local` da pasta do projeto do site (`lint_case::project_dir_for_root`: a raiz de documentos, o pai ou o avô com marcador de projeto — a mesma regra do terminal e do estado case-sensitive), inclusive sites ligados fora de `www/`. Nada de expansão das raízes do `fs.*` (que continuam `etc/`, `logs/`, `www/`).
- Abertura sem seguir links (`open_no_follow`: link simbólico, junction ou arquivo com mais de um hard link é deixado de fora e logado), no máximo 256 KiB por arquivo, UTF-8 com perdas. Proxy (sem pasta) → lista vazia.
- Parser (`forge_sites::dotenv`): a gramática comum (phpdotenv, `dotenv` do Node, Symfony): `KEY=valor`, `export`, comentários, aspas duplas com escapes e várias linhas, aspas simples literais, ` #` após valor sem aspas.
- Segredo (`is_secret`): chave contendo `PASS`, `SECRET`, `KEY`, `TOKEN`, `PRIVATE`, `CREDENTIAL` ou `SALT` (sem caixa), **ou** valor que é URL/DSN com credencial (`mysql://u:p@h/db`, `https://chave@sentry.io/1`). Valores secretos vêm `null` sem `reveal`: mascarados nunca saem do daemon. `reveal` sem `admin` → `forbidden` (conferido no handler; a tabela tem um papel por método); com `admin` gera linha de auditoria no log.

## Análise de ameaças
| Ameaça | Mitigação |
|---|---|
| Processo do usuário pede ao helper (um UAC) para remover uma raiz corporativa ou da Microsoft de `LocalMachine\Root` | `trust.remove_machine_root` só remove certificado que passaria em `trust.add_machine_root`; CN `Forgebox...` sem restrições também é recusado |
| Cliente remoto (operador via WebSocket) encerra processos da máquina do daemon | `local_only` recusa antes do handler (`forbidden`); `admin` exigido |
| GUI/CLI enganada (ou corrida) mata processo errado porque o pid foi reciclado | Checagem e `TerminateProcess` no mesmo handle; porta conferida de novo com o handle aberto |
| Usar o "Encerrar" para derrubar o próprio Forgebox, um serviço do Windows (`MySQL80`, `W3SVC` em `System`), `csrss`/`wininit` (tela azul) ou processos de outro usuário | `KillPolicy`: daemon, árvore de Jobs, raiz e nomes do Forgebox, críticos, serviços, sessão 0, contas de sistema, outro usuário/token ilegível, `%SystemRoot%` |
| Daemon elevado por engano amplia o alcance do kill | Mesmo elevado, a política recusa contas de sistema, sessão 0, serviços e outros usuários |
| Encerramento sem rastro | Evento `port.holder_killed` (persistido no histórico de eventos) + log `warn` com `client`; recusas logadas |
| `sites.env` como leitor genérico de arquivos (`../../`, `C:\Users\x\.ssh\id_rsa`) | Pedido só com site; nomes de arquivo fixos; pasta derivada da raiz do site; links e hard links recusados; limite de tamanho |
| `.env` com junction/symlink plantado apontando para outra pasta | `open_no_follow` recusa reparse point no último componente; o arquivo fica de fora |
| Segredos de `.env` vazando para cliente remoto, logs ou telemetria | Mascaramento no daemon (`value: null`), `reveal` só `admin`; valores nunca são logados (só "revealed" com site e cliente) |
| Segredo em chave "inocente" (`DATABASE_URL`, `SENTRY_DSN`) | Valor com credencial em URL/DSN também é segredo |

## Alternativas rejeitadas
- **Nova op do helper (`trust.uninstall_ca`):** duplicaria `trust.remove_machine_root` (mesmo efeito, mesmo parâmetro) e aumentaria a allowlist fechada; endurecer a validação da op existente dá a "mesma validação da instalação" sem crescer a superfície. A rotação, que também remove a CA antiga da máquina, ganha a mesma proteção.
- **Apagar os arquivos da CA ao desinstalar:** quebraria os sites (folhas órfãs) e a rotação; a remoção dos arquivos é do desinstalador do app (docs/08 §3).
- **`taskkill /PID`:** shell e saída localizada, sem a checagem no mesmo handle (janela de reciclagem de pid).
- **Encerrar também processos órfãos do Forgebox (executável em `bin/`) que seguram a porta:** útil depois de um crash, mas o caminho certo é `forge stop`/reiniciar o daemon (que adota ou limpa os seus); deixar o "Encerrar" matar binários da raiz abriria a porta para matar serviços gerenciados por fora do supervisor. A mensagem orienta.
- **Recusar só por sessão 0 (sem enumerar serviços):** serviços por usuário (`CDPUserSvc_*`) rodam na sessão do usuário; a enumeração do SCM os pega e nomeia o serviço na mensagem.
- **Seguir links do `.env` e conferir o caminho final dentro do projeto:** mais permissivo para monorepos com `.env` linkado, mas a checagem de "dentro do projeto" depende de uma raiz canônica que o próprio usuário pode redirecionar; recusar link é mais simples de auditar. Fica como pedido futuro se aparecer.
- **Mascarar parcialmente (`mysql://root:****@...`):** o DTO não tem campo para isso e um formato parcial é fácil de errar; `null` + `secret: true` é inequívoco.
- **`sites.env_reveal` separado:** já rejeitado na ADR-0040.

## Consequências
- Fecha o problema conhecido da 0.1.0 ("sem desinstalar a CA pela API"); `forge tls ca uninstall [--scope user|machine]`, `forge service kill-port <porta> --pid <n>` e `forge site env <host> [--reveal]` deixam de responder `not_implemented`.
- `forge-core`: `platform::process_kill` (`KillPolicy`, `ProcessFacts`, `ProcessTerminator`, `process_terminator()`), `Platform::with_process_terminator`/`process_terminator`, `platform::windows::WindowsProcessTerminator`, `MemoryTrustStore`; feature nova do crate `windows`: `Win32_System_Services`. `forge-sites`: `dotenv`. `forge-daemon`: `BootOptions::trust_store`; o helper do daemon usa a elevação da `Platform`.
- Um processo do usuário que use `%SystemRoot%` (raro para quem segura uma porta de dev) não pode ser encerrado pelo Forgebox; a mensagem diz por quê e a alternativa "Trocar porta" continua.
- Testes: política com fatos fabricados (crítico, serviço, sessão 0, sistema, outro usuário, raiz, nomes); terminador real contra um filho (`ping`); daemon em processo matando um ouvinte filho real, recusando pid que não segura a porta, o próprio daemon, sem `confirm` e remoto, com o evento de auditoria; `tls.ca_uninstall` com `MemoryTrustStore` e `FakeElevation` (idempotente, evento, máquina pelo helper); `sites.env` mascarando `DB_PASSWORD`/`APP_KEY`, `reveal` sem `admin` → `forbidden`, nada além dos dois arquivos alcançável.
