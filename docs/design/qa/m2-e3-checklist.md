# Checklist docs/07 §11 das telas do E3 (M2-E3-T21)

Seis telas do E3: **Logs**, **Mail**, **Bancos**, **Terminal**, **Configurações** (Geral, Rede, Segurança, Sistema de arquivos, Registries e Atualizações) e **Diagnóstico**, mais as abas de site do E3: **Banco** (T14), **Ambiente** (T20) e **Processos** (botão Logs, T12). Este documento é a evidência do critério 3 da tarefa; o que ficou aberto vai para o M2-E4 (seção "Pendências"). O relatório vivo do M2-E4 é [M2-E4-report.md](M2-E4-report.md).

## Como foi medido

| Fonte | Comando | Resultado (2026-11-04, esta máquina, sessão RDP, Edge 156) |
|---|---|---|
| Regras automáticas do §11 sobre o fakeBridge | `FORGE_QA_CHECK_PORT=4276 MSYS_NO_PATHCONV=1 pnpm -C apps/desktop exec playwright test -c playwright.qa.config.ts --grep "(/databases\|/mail\|/logs\|/terminal\|/settings\|/diagnostics\|site tab)"` | **157 de 157 passam** (140 numa execução e 17 do `/databases` noutra: sob Git Bash sem `MSYS_NO_PATHCONV` o primeiro termo do regex é reescrito como caminho e a rota some do filtro, o que só foi notado ao conferir a contagem por rota): 17 casos por rota nas 6 rotas (axe 4, contratos 8, teclado 2, Escape 1, CLS 1, reduced-motion 1), mais 16 casos de matriz do shell/acento nas rotas e a spec nova `qa/checks/site-tabs.spec.ts` (39 casos: axe em claro/escuro x vibrancy on/off, contratos nos 8 cenários e teclado, para Banco, Ambiente e Processos) |
| Lint estático | `pnpm -C apps/desktop lint` e `typecheck` | 0 erros no projeto inteiro |
| Matriz de capturas (claro/escuro x DPR 1, 1,25 e 1,5 x vibrancy on/off x reduced x en-US x pseudo x 960x540 a 1280x800) | `FORGE_QA_SHOTS_PORT=4277 pnpm -C apps/desktop qa:shots --grep "(databases\|mail\|logs\|terminal\|settings\|diagnostics\|site-detail)"` | **140 células, 7 folhas de contato** em `qa/out/shots/20261104T163405/sheets/` (ignorado pelo git); as folhas de mail e terminal foram lidas em visão geral, sem zoom por célula. As abas de site não estão na matriz (ela abre só a aba padrão): `site-tabs.spec.ts` grava `qa/out/site-tabs/<aba>-<tema>.png` (6 capturas 1280x720, lidas: Banco claro e Ambiente escuro) |
| Daemon real (suíte live do E3, 7 testes, ~22 s) | `powershell -NoProfile -File scripts/gui-e2e.ps1 -Suite e3` | **7 de 7 passam**; o script verifica que não sobra `Forgebox.exe`, daemon, `msedgewebview2`, serviço falso nem a pasta temporária (nenhum aviso de "left behind" nas 3 execuções que chegaram ao fim) |
| Suíte live do E2 (regressão do fixture compartilhado) | `powershell -NoProfile -File scripts/gui-e2e.ps1` | 9 de 9 passam (Start no tile 1,5 ms; kill do daemon visto em 146 ms) |
| Varredura de paridade | cenário `parity` da suíte do E3 (mesmo validador do E2) | **14 ações `[data-action]` em 7 visões** das rotas do E3, 0 problemas (o `forge` real aceita cada linha) |

## Suíte live do E3

`apps/desktop/e2e/live/e3/e3.live.spec.ts`, sobre o mesmo fixture do E2 (`e2e/live/fixture.mjs`, agora com o cliente de banco falso `fake_db_client` como `mysql.exe`/`mysqldump.exe` e um projeto `queue` para o Logs) e sobre `crates/forge-daemon/examples/live_daemon.rs`: o mesmo boot do `forgeboxd`, mas com `MemoryTrustStore`, `MemoryUserEnv` e `FakeElevation`, para que o fluxo "Corrigir" do doctor (CA e PATH) rode de ponta a ponta sem escrever em nenhum repositório de certificados, NRPT ou HKCU. A raiz do E3 não tem `portable.flag` (o modo portátil recusa instalar a CA e pula a checagem de PATH); como o armazenamento é em memória, nada chega ao Windows.

| # | Cenário | O que prova (medido) |
|---|---|---|
| 1 | Mail com `fake_mailpit` | 3 mensagens injetadas -> selo `badge-mail` = 3 e 3 linhas na lista, sem navegar (`mail.changed`); abrir uma mensagem mostra o assunto e o selo cai para 2 junto com `mail.status.unread`; Apagar esconde a linha na hora e a chamada só sai após a janela de 5 s (`mail.status.messages` continua 3 e depois vira 2); nova injeção move o selo para `unread + 1` |
| 2 | Terminal | "Novo terminal" abre a sessão no daemon (ConPTY real); `echo FORGE_E3_ECHO > <arquivo>` digitado no emulador cria o arquivo com o texto (as teclas chegaram ao shell); `exit` mostra o banner "Process ended" |
| 3 | Logs de site | `sites.up` do projeto `queue`; com um `access.json` de fixture (4 linhas, 3 do site e uma de outro host), `/logs?target=site:queue.test` mostra só as 3 (host e alias), com as colunas de status `200, 404, 503` e sem a linha do outro host |
| 4 | Doctor com backends falsos | CA instalada e removida (memória); "Executar diagnóstico" mostra `tls.ca_trusted` em `warn`; "Corrigir" roda `tls.ca_install` e a linha vira `ok` no lugar; `tls.ca_status.trusted_current_user` fica `true` |
| 5 | Config ao vivo | `system.config_set editor.command` por outro cliente aparece no campo aberto de Configurações > Editor/Terminal sem recarregar (`config.changed`), e o `forgebox.toml` do disco tem o valor |
| 6 | Bancos com o cliente falso | Criar `shop` pelo formulário (`databases.txt` do cliente falso o contém); export de 64 MiB a ~200 ms/MiB cancelado pelo botão (texto "Export cancelled.", nenhum `shop-*` em `backups/`); export de 4 MiB conclui ("Exported to", um `.sql.zst`); import de um `.sql` pelo diálogo conclui ("Import finished.") e o `restored.bin` do cliente falso tem exatamente os bytes do arquivo |
| 7 | Paridade | 14 ações em 7 visões com comando validado |

## Checklist por tela

Legenda: **ok** = evidência na seção "Evidência"; **ok\*** = ok com a ressalva citada em "Pendências". Item 1: tokens; 2: estados loading/sucesso/erro com texto; 3: erro com causa + ação; 4: claro/escuro/vibrancy-off; 5: teclado e foco visível; 6: comando `forge` equivalente; 7: 125 % e 150 % DPI; 8: sem layout shift.

| # | Item | Logs | Mail | Bancos | Terminal | Configurações | Diagnóstico | Aba Banco | Aba Ambiente | Aba Processos |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | Tokens, sem cor literal | ok | ok | ok | ok | ok | ok | ok | ok | ok |
| 2 | loading/sucesso/erro com texto | ok | ok | ok | ok | ok | ok | ok | ok | ok |
| 3 | Erro com causa + ação | ok | ok | ok | ok | ok | ok | ok | ok | ok |
| 4 | Claro / escuro / vibrancy-off | ok | ok | ok | ok | ok | ok | ok | ok | ok |
| 5 | Teclado e foco visível | ok | ok | ok | ok | ok | ok | ok\* | ok | ok |
| 6 | Comando `forge` equivalente | ok | ok | ok | ok | ok | ok | ok | ok | ok |
| 7 | 125 % e 150 % DPI | ok\* | ok\* | ok\* | ok\* | ok\* | ok\* | n/a | n/a | n/a |
| 8 | Sem layout shift | ok | ok | ok | ok | ok | ok | ok\* | ok\* | ok\* |

### Evidência

1. **Tokens.** `pnpm lint` (regras de cor, i18n, `eslint-disable` e stylelint) passa sem erro; as folhas de contato mostram claro, escuro e o tema de contraste forçado (informativo) sem cor fora dos tokens.
2. **Estados com texto.** Ao vivo: Mail mostra a lista vazia, depois 3 linhas por evento; o Apagar tem a janela de desfazer de 5 s; Terminal mostra "Process ended"; Doctor mostra o progresso e o resumo, e a linha passa de `warn` a `ok`; Bancos mostra `written`, "Export cancelled.", "Exported to" e "Import finished." (cenário 6). Os estados vazio/erro/carregando são exercidos pelos 8 cenários do fakeBridge nos contratos.
3. **Erro com causa + ação.** `contracts: <rota> scenario errors|daemon-down|not-implemented` exige `data-error-code`, causa e ao menos um botão em todo `[data-error-notice]`: 64 casos passam nas 6 rotas e nas 3 abas (24 casos novos; 72 contratos no total).
4. **Temas.** axe WCAG 2.1 AA com 0 serious/critical em claro e escuro, vibrancy on e off, nas 6 rotas e nas 3 abas (24 casos de rota e 12 de aba); células `light`, `dark` e `viboff` das folhas.
5. **Teclado.** `tab order and focus ring` (rotas) e `keyboard: site tab ...` (abas, que abrem a aba com Enter e depois percorrem a página com Tab). **Achado corrigido nesta tarefa:** o link "Abrir em Bancos" da aba Banco não tinha anel de foco (`FOCUS_RING`); a spec nova o pegou e a correção está em `DatabaseTab.tsx`. A ressalva (ok\*) da aba Banco é só essa correção ter entrado neste PR.
6. **Comando `forge`.** `contractViolations` (todo `[data-action]` com `data-forge-cli`) e a varredura ao vivo de 14 ações em 7 visões, aceitas pelo `forge` real.
7. **DPI.** Células 1,25x e 1,5x (claro e escuro) das folhas, mais 2x informativa, lidas em visão geral (mail e terminal): nenhuma quebra de layout vista; não houve inspeção célula a célula em tamanho real (ver pendências). As abas ficam fora da matriz (n/a).
8. **Layout shift.** `CLS <rota>` com dados atrasados: 6 casos nas rotas do E3, todos < 0,01. As abas não têm caso de CLS dedicado (ok\*): são montadas sob demanda dentro da página de detalhe, que já tem caso.

## O que a suíte live não cobre

- **Mailpit e MySQL reais:** não houve notas manuais (o usuário não estava disponível durante a tarefa). Ficam para o M2-E4-T11. `fake_mailpit` e `fake_db_client` são os dublês de `crates/forge-daemon/tests/fixtures`.
- **Eco do terminal:** o buffer do xterm não é legível no build de produção (WebGL não desenha DOM; o gancho `__FORGEBOX_TERM__` só existe no build `qa`), então o eco é provado pelo efeito no disco (o shell escreve o arquivo) e o banner de saída pela tela.
- **Correção de DNS do doctor:** `net.dns_enable` mexe na NRPT, que não tem backend falso injetável no `forgeboxd` real; fica coberta pelos testes de unidade da allowlist e do daemon. `tls.ca_install` (repositório em memória) e `shell.path_add` (`MemoryUserEnv`) rodam ponta a ponta.
- **`qa:check` filtrado:** rodado só nas rotas e abas do E3 (157 casos), não a suíte inteira (verificação leve; o gate de lote roda tudo).

## Pendências para o M2-E4

| ID | Sev. | Tela | Achado | Evidência |
|---|---|---|---|---|
| E3-01 | P3 | Janela real | A suíte live roda na sessão RDP (`data-vibrancy=off`); as telas do E3 não foram comparadas com o protótipo de vidro na janela real. | `qa/out/gui-e2e/native-frame.png` |
| E3-02 | P3 | Terminal, Mail, Bancos | Folhas de contato lidas em visão geral; sem inspeção célula a célula em 1,25x/1,5x. | `qa/out/shots/20261104T163405/sheets/` |
| E3-03 | P3 | Testes (harness) | `keyboardViolations` marca o `div[role=tablist]` (sem anel) quando a aba é aberta por clique do mouse e a varredura de Tab começa em seguida (visto também na aba Geral); abrindo a aba com Enter não ocorre. É um artefato de como o Radix redireciona o foco do grupo, não do produto, mas vale uma regra do harness que ignore o contêiner roving. | `qa/checks/site-tabs.spec.ts` (comentário no caso de teclado) |
| E3-04 | P3 | Mailpit/MySQL reais | Verificação manual do Mail (badge, apagar, HTML real) e de Bancos (create/export/import com MySQL real) não feita. | seção "O que a suíte live não cobre" |

Não ficaram pendências P0 ou P1 para as seis telas e as três abas.
