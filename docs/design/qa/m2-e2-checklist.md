# Checklist docs/07 §11 das telas do E2 (M2-E2-T20)

Seis telas do E2: **Onboarding**, **Dashboard**, **Serviços**, **Sites** (lista e detalhe), **PHP** e **Catálogo** (lista e página da ferramenta). Este documento é a evidência do critério 4 da tarefa; o que ficou aberto vai para o M2-E4 (seção "Pendências"). O relatório vivo do M2-E4 é [M2-E4-report.md](M2-E4-report.md).

## Como foi medido

| Fonte | Comando | Resultado (2026-11-04, esta máquina, sessão RDP, Edge 156) |
|---|---|---|
| Regras automáticas do §11 sobre o fakeBridge | `pnpm -C apps/desktop exec playwright test -c playwright.qa.config.ts --grep "(/dashboard\|/services\|/sites\|/php\|/catalog\|/onboarding)"` | **168 de 168 passam**: axe 32, contratos DOM 64, teclado 24, CLS 8, reduced-motion 8, matriz do shell 32 (as 6 telas são rotas de `ROUTES`; a lista traz também `/sites/<id>`, `/catalog/php` e `/catalog/laravel`) |
| Lint estático (tokens, cores literais, < 11 px, texto literal, `eslint-disable` sem motivo) | `pnpm -C apps/desktop lint` | 0 erros no projeto inteiro |
| Matriz de capturas (claro/escuro x DPR 1, 1,25 e 1,5 x vibrancy on/off x reduced x en-US x pseudo x tamanhos 960x540 a 1280x800) | `pnpm -C apps/desktop qa:shots --grep "(onboarding\|dashboard\|services\|sites\|site-detail\|php\|catalog)"` | **160 células, 8 folhas de contato** em `qa/out/shots/20261104T145124/sheets/` (ignorado pelo git); as 8 folhas foram lidas (visão geral, sem zoom por célula) |
| Daemon real (suíte live, 9 testes, ~33 s) | `powershell -NoProfile -File scripts/gui-e2e.ps1` | **9 de 9 passam**, nenhum processo sobra (o script verifica `Forgebox.exe`, `forgeboxd`, `msedgewebview2`, serviços falsos e a pasta temporária) |
| Varredura de paridade | cenário `parity` da suíte live + `e2e/live/parity.test.ts` (3 testes do validador) | **81 ações `[data-action]` em 13 visões** das rotas do E2, 0 problemas |

Medidas da suíte live (6 execuções que chegaram ao cenário 2 e 2 execuções completas verdes, `--SkipBuild`, debug): Start no tile até o "Starting" na tela **1,1 a 1,8 ms** (marca `forge:start-click` até `forge:start-shown`, limite 50 ms); kill do daemon até o primeiro status diferente de `connected` **138 e 185 ms** nas 2 execuções completas (limite 2 s), e a tela volta a `connected` com o mesmo documento (marcador `window.__liveMarker` intacto, sem reload).

## Checklist por tela

Legenda: **ok** = evidência na coluna "Evidência"; **ok\*** = ok com a ressalva citada em "Pendências". Item 1: tokens; 2: estados loading/sucesso/erro com texto; 3: erro com causa + ação; 4: claro/escuro/vibrancy-off; 5: teclado e foco visível; 6: comando `forge` equivalente; 7: 125 % e 150 % DPI; 8: sem layout shift.

| # | Item | Onboarding | Dashboard | Serviços | Sites | PHP | Catálogo |
|---|---|---|---|---|---|---|---|
| 1 | Tokens, sem cor literal | ok | ok | ok | ok | ok | ok |
| 2 | loading/sucesso/erro com texto | ok | ok | ok | ok | ok | ok |
| 3 | Erro com causa + ação | ok | ok | ok | ok | ok | ok |
| 4 | Claro / escuro / vibrancy-off | ok | ok | ok | ok | ok | ok |
| 5 | Teclado e foco visível | ok | ok | ok | ok | ok | ok |
| 6 | Comando `forge` equivalente | ok | ok | ok | ok | ok | ok |
| 7 | 125 % e 150 % DPI | ok\* | ok | ok | ok | ok | ok |
| 8 | Sem layout shift | ok | ok | ok | ok | ok | ok |

### Evidência

1. **Tokens.** `pnpm lint` (regras `eslint.colors.js`, `eslint.i18n.js`, `eslint.disable.js`, stylelint) passa sem erro em todas as telas; as folhas de contato mostram claro, escuro e o tema de contraste forçado (informativo) sem cor fora dos tokens.
2. **Estados com texto.** `ActionButton` expõe `data-phase` (`idle|running|done|failed`) e o texto do estado. Medido no daemon real: o Start do tile mostra "Starting" em 1,1 a 1,8 ms (Dashboard, cenário 2); o Apply do quick setting passa por `running` e termina sem `failed` e com `my.ini` alterado (Serviços, cenário 5); a troca de PHP mostra "Switching to…" e "PHP 8.3 is active" (Sites, cenário 4); o toggle de extensão fica pendente e confirma com o overlay `10-forgebox.ini` (PHP, cenário 6); a instalação do catálogo mostra o job e termina em `data-state="cancelled"` ao cancelar (Catálogo, cenário 7); cada passo do onboarding mostra `data-status` (cenário 1).
3. **Erro com causa + ação.** `contracts: <rota> scenario errors` (e `daemon-down`, `not-implemented`) exigem `data-error-code`, texto de causa e ao menos um botão em todo `[data-error-notice]`: 64 casos passam nas rotas do E2. Ao vivo: ao matar o daemon o status `reconnecting` chega à interface em 138 a 185 ms (cenário 8, evento `forge://connection`).
4. **Temas.** axe WCAG 2.1 AA (0 serious/critical) em claro e escuro, vibrancy on e off, por rota (32 casos) e as células `light`, `dark` e `viboff` das folhas de contato.
5. **Teclado.** `tab order and focus ring: <rota>` (24 casos): Tab alcança todo controle com anel `--accent`, Escape fecha e devolve o foco. O onboarding tem ainda o caminho Laravel só com teclado (`e2e/onboarding.spec.ts`, M2-E2-T19).
6. **Comando `forge`.** `contractViolations` (todo `[data-action]` com `data-forge-cli`) nas rotas e cenários do fakeBridge, e a varredura ao vivo: as 81 ações do daemon real passam por quatro verificações (ver "Varredura de paridade").
7. **DPI.** Células 1,25x e 1,5x (claro e escuro) das 8 folhas, mais a de 2x informativa. As folhas foram lidas em visão geral: nenhuma célula quebra o layout. Não houve inspeção célula a célula em tamanho real (ver pendências).
8. **Layout shift.** `CLS <rota>` com dados atrasados em 300 ms: 8 casos, todos com CLS < 0,01.

## Varredura de paridade (critério 3)

`apps/desktop/e2e/live/parity.ts` lê, em cada visão que a suíte visita, todo `[data-action]` e exige: (a) `data-forge-cli` não vazio; (b) o id da ação existe em `qa/parity/gui-commands.json` (o golden, cujos comandos `crates/forge-cli/tests/gui_parity.rs` parseia com o clap real e mapeia de volta ao método); (c) o primeiro verbo do comando renderizado é um dos que o golden tem para essa ação; (d) o binário `forge` real aceita a linha (`forge <args> --help` sai com 0, ou seja, o clap resolveu subcomandos, flags e posicionais daquela linha). Visões: onboarding (stack, opções, execução), Dashboard, Serviços (Visão geral, Logs, Instâncias, Configuração, Versões, Avançado), Sites (novo site, lista), detalhe do site (Geral, Processos, Ambiente, Diagnóstico), PHP, Catálogo e página da ferramenta. Resultado: **81 ações, 0 problemas**. O validador tem teste negativo em `e2e/live/parity.test.ts` (ação sem comando, ação fora do golden, verbo trocado e linha rejeitada pelo clap).

## Pendências para o M2-E4

| ID | Sev. | Tela | Achado | Evidência |
|---|---|---|---|---|
| E2-01 | P2 | Dashboard (Atenção) | O aviso de porta em uso mostra `A porta 3389 está em uso por ?` (a porta é a da sessão RDP desta máquina): o holder chega com executável `?`, então `attention.ts` usa `attention.ports.title` com `?` em vez de `titleUnknown`. Quando o dono não foi identificado, o texto deve ser o de `titleUnknown`. | `src/features/dashboard/attention.ts` (linhas 61 a 63); texto lido do corpo da página na primeira exploração do app real |
| E2-02 | P2 | Configurações > Geral (E3, visto ao dirigir o app real) | O idioma escolhido no seletor não é lido na inicialização: `src/main.tsx` só passa `override` vindo do handle QA, então ao reabrir o app volta ao idioma do SO. A suíte live troca para en-US a cada sessão por esse caminho. | `src/main.tsx`, `LanguageSwitcher.tsx` |
| E2-03 | P3 | Onboarding, 960x540 e 1093x574 | O botão Continuar fica na borda inferior (a página rola); em 960x540 só aparece parte dele. Visto na folha `onboarding.png`, sem medição em tamanho real. | `qa/out/shots/20261104T145124/sheets/onboarding.png` |
| E2-04 | P3 | Sites > detalhe, 960x540 | O bloco "comando equivalente" fica cortado na borda inferior (a página rola). Mesma observação, sem medição em tamanho real. | `sheets/site-detail.png` |
| E2-05 | P3 | Testes (e2e/real) | `e2e/real/app.spec.ts` procura `data-testid="connection-status"` e só asserta se existir; o rodapé usa `status-connection`, então a checagem nunca roda. | `src/app/shell/SidebarStatus.tsx` |
| E2-06 | P3 | Dashboard (Iniciar) | O botão de loading tem o texto do estado e o do rótulo no DOM (`StartStarting` em `textContent`, um deles oculto). Para leitores de tela só o visível deveria ser lido; conferir o `aria` com NVDA. | `PERF` da suíte live |
| E2-07 | P3 | Janela real | A suíte live roda na sessão RDP (`data-vibrancy=off` esperado) e a janela real não foi comparada com o protótipo de vidro (M2-E1-T23). | `qa/out/gui-e2e/native-frame.png` |

Não ficaram pendências de severidade P0 ou P1 para as seis telas.
