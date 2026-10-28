# ADR-0035 — Moldura da janela, materiais e a fronteira Win32 da GUI

**Status:** aceito · **Data:** 2026-10-28 · Implementa os materiais e a titlebar de [07](../07-ui-ux.md) §1.6, §2.3, §4 e [ADR-0002](0002-gui-tauri.md); detalha os dados da GUI da [ADR-0034](0034-desktop-shell-architecture.md) §5; segue o precedente de `spawn_detached` da [ADR-0030](0030-cli-contract.md) para fatos do SO. Tarefa M2-E1-T7.

## Contexto
O `Forgebox.exe` desenha a própria titlebar (janela sem decoração do Windows) e quer Mica/Acrylic onde ajudam, sem flash branco na abertura, lembrando a posição por monitor, com instância única e com os dados da GUI fora do `forgebox.toml`. Três armadilhas conhecidas guiaram as decisões:

- o `set_effects` do Tauri responde `Ok` no Windows 10 sem aplicar nada, e numa sessão RDP o material renderiza preto ou lento — a decisão não pode confiar nele;
- o Acrylic do Windows 10 (1903+) é documentado pelo próprio `window-vibrancy` como "a janela atrasa ao redimensionar ou arrastar";
- o CLAUDE.md proíbe Win32 fora de `forge-core::platform::windows`, e a GUI só depende de `forge-ipc`.

## Decisão
**1. Fronteira Win32 da GUI.** `apps/desktop/src-tauri` não depende de `windows`/`windows-sys`/`windows-core` (guarda `cargo xtask arch-guard`, que roda no CI). Pode usar o crate `window-vibrancy` (o mesmo 0.8.1 que o Tauri já linka) e plugins do Tauri (`tauri-plugin-single-instance`), que trazem o próprio Win32. Fatos do SO vêm de traits do forge-core reexportadas por `forge_ipc::base::platform`: `SystemAppearance::snapshot()` (build, RDP, "Efeitos de transparência", alto contraste, cor de destaque, tema dos apps e da barra) e `process_creation_time`. Scripts de QA (`scripts/qa/window-probe.ps1`) podem usar Win32 por PowerShell: não são código do produto.

**2. Política de material** — `choose_effect(AppearanceSnapshot, prefs) -> {Mica | Acrylic | None, motivo}`, função pura (`src-tauri/src/window.rs`), nesta ordem:

| condição | efeito | motivo |
|---|---|---|
| preferência `vibrancy: "off"` | opaco | `user_off` |
| alto contraste | opaco | `high_contrast` |
| sessão remota (RDP) | opaco | `remote_session` |
| "Efeitos de transparência" desligado | opaco | `transparency_off` |
| Windows 11 (build ≥ 22000) | Mica | `win11` |
| Windows 10 1809+ com `win10_acrylic: true` | Acrylic | `win10_opt_in` |
| Windows 10 1809+ sem opt-in | **opaco** (`WIN10_DEFAULT`) | `win10_default` |
| mais antigo/desconhecido | opaco | `unsupported` |

O efeito escolhido é aplicado por `window_vibrancy::apply_mica|apply_acrylic` e o **`Result` decide**: erro → `clear_*` e opaco (`apply_failed`). `set_effects` do Tauri nunca é usado. A política é reavaliada (snapshot em `spawn_blocking` com prazo de 2 s, aplicação na thread principal, resultados antigos descartados por geração) quando o tema muda (`ThemeChanged`), quando a janela ganha foco (cobre transparência, alto contraste e reconexão RDP, que não têm evento no Tauri) e quando uma preferência muda (`gui_prefs_set`). `FORGE_GUI_EFFECT=mica|acrylic|none` força a tentativa (QA e a medição abaixo), ainda sujeito ao `Result`.

**3. Janela transparente só com material.** A janela é criada **transparente apenas se um material é esperado na partida**; senão é opaca com a cor `bg-canvas` do tema (`docs/design/tokens.json`, conferida por teste) como fundo da janela e do WebView2. Medido nesta máquina (Win10 19045, RDP): com a janela sempre transparente, o primeiro quadro visível foi preto em 3 de 8 aberturas (o WebView2 ainda não tinha apresentado; a área transparente sem material é preta) — com a janela opaca até haver material, 0 de 10 (nenhum quadro preto ou branco; em 4 o primeiro quadro ainda era só o canvas, com o conteúdo 100 ms depois — a mesma cor de fundo, sem clarão). Consequência: numa janela aberta opaca, um material que fica possível depois (sair do RDP para o console, ligar o opt-in) só vale na próxima abertura (motivo `restart_required`).

**4. Primeiro quadro.** A janela é criada no `setup` (não no `tauri.conf.json`) e **escondida**: assim recebem a pasta do perfil do WebView2, um script de inicialização e a posição restaurada antes de aparecer. O script publica `window.__FORGEBOX_SHELL__ = {appearance, minimized}`; `src/app/appearance.ts` aplica `data-theme`, `data-vibrancy`, `data-effect`, `data-contrast` e `--system-accent` em `<html>` antes do primeiro paint, depois segue o evento `forge://appearance` (e pergunta `appearance_get` depois de assinar, para não perder uma mudança). O primeiro componente do shell chama `useFirstFrameReady()`: depois do commit, um duplo `requestAnimationFrame` e o comando **`ready`**, que mostra a janela (maximizada se era) e a foca — ou a mantém escondida com `--minimized`. Um vigia mostra a janela após 5 s sem `ready` (página quebrada nunca fica invisível). `perf.rs` marca criação do processo (`process_creation_time`) → `ready`; o comando `perf_startup` devolve as marcas e `Forgebox.exe --perf-startup` imprime uma linha JSON `{"first_frame_ms", "origin", "process_created_unix_ms", "ready_unix_ms", "session", "os_build", "effect", "version"}` no stdout e sai 0 (sem `ready` em 30 s: `first_frame_ms: null`, `error: "ready_timeout"`, saída 1).

**5. Titlebar e controles** (`src/app/shell/Titlebar.tsx`, `WindowControls.tsx`). 32 px, transparente sobre o material/canvas. Arrasto por `data-tauri-drag-region="deep"` no `<header>` (o script do Tauri ignora botões e elementos interativos; duplo clique maximiza/restaura pelo `internal_toggle_maximize`). Botões 46×32 como os do Windows (glifos SVG de 10 px; fechar com hover `--state-failed`), `aria-label` e tooltip nativo (`title`, como os botões de legenda do próprio Windows), ordem de Tab busca → minimizar → maximizar → fechar, anel `--accent`. Slot da busca `Ctrl+K` (`aria-keyshortcuts`), que dispara `forgebox:open-palette` até a paleta (M2-E1-T14) ocupar o slot. A janela usa a borda de redimensionar invisível do Tauri (8 px fora da área visível nas laterais/base; no topo, os primeiros pixels da titlebar) e a sombra do sistema (`shadow: true`). Capabilities: `core:window:allow-start-dragging|minimize|toggle-maximize|close`, só na janela `main`.

**6. Snap layouts do Windows 11 — spike com limite de tempo, adiado para o gate.** O menu aparece quando o Windows vê `HTMAXBUTTON` no hit-test da janela; com o WebView2 cobrindo a área cliente isso exige Win32 próprio (subclass/`WM_NCHITTEST`) ou regiões não-cliente do WebView2, nenhum disponível sem Win32 no `src-tauri`. Avaliado o único plugin pronto, `tauri-plugin-decorum` 1.1.1: **rejeitado** — injeta a própria titlebar por `eval` (duplicaria e brigaria com a nossa, acessível e traduzida), depende de `window.__TAURI__` global (desligado aqui) e o "snap" é simular `Win+Z` pelo `enigo` (`SendInput` de teclado global após 620 ms de hover). Arrastar para as bordas (Aero Snap) e `Win+setas` funcionam (a janela mantém `WS_THICKFRAME`). Verificação em Windows 11 e a solução (regiões não-cliente do WebView2 quando o wry expuser, ou um plugin que as use) ficam para o gate M2 (M2-E4-T11, "Verificação humana").

**7. Posição por monitor** (`window_state.rs`, `window-state.json` nos dados da GUI). Uma posição por monitor (nome + limites físicos), a mais recente primeiro (até 8): na abertura vale a do monitor conectado mais recente, senão a de outro monitor conhecido, senão 1200×800 centralizada; sempre encaixada na área de trabalho, reescalada se o DPI do monitor mudou, e no mínimo **min(960×600, área de trabalho)** (recalculado em `ScaleFactorChanged`). Maximizada volta maximizada (o `maximize()` com a janela escondida só marca o estado; o `show` abre maximizada, sem flash). Gravada ao fechar e na saída; nunca em `--perf-startup` nem quando a janela nunca apareceu (`--minimized`). **`tauri-plugin-window-state` rejeitado**: todo `save` faz `create_dir_all(app_config_dir)` (`%APPDATA%\dev.forgebox.app`) mesmo com caminho absoluto — escrita fora da raiz no modo portátil — e guarda uma posição por janela, não por monitor.

**8. Dados da GUI** (`data_dir.rs`). `FORGE_GUI_DATA_DIR` → raiz portátil (a raiz descoberta como o `forge` faz tem `portable.flag`) → `<raiz>/state/gui` → `%LOCALAPPDATA%\Forgebox\gui` → `%TEMP%\Forgebox\gui`. Dentro: `prefs.json`, `window-state.json` e `webview/` (pasta de dados do WebView2, `WebviewWindowBuilder::data_directory`). As pastas padrão do Tauri (`%APPDATA%`/`%LOCALAPPDATA%\dev.forgebox.app`) não são criadas nem tocadas (verificado pelo `scripts/window-qa.mjs`). Como no portátil a GUI pode criar `state/gui` antes do primeiro boot do daemon, a ponte (ADR-0034 §3) passa a considerar a raiz inicializada só quando `state/` tem algo **além** de `gui/` (`root_initialized`).

**9. Preferências da GUI** (`prefs.rs`, comandos `gui_prefs_get`/`gui_prefs_set`). `theme` (`system|light|dark`), `vibrancy` (`auto|off`), `win10_acrylic` (bool, padrão `false`), `close_to_tray` (padrão `true`); qualquer outra chave de nível superior é preservada para as features donas dela (idioma, atalhos, notificações...). `gui_prefs_set` faz merge (`null` volta ao padrão), valida (`invalid_input`), grava atomicamente (`.tmp` + rename) em `spawn_blocking` e reaplica tema/material.

**10. Fechar e instância única.** Fechar esconde na bandeja só com `close_to_tray` **e** uma bandeja existente (`WindowShell::set_tray_available`, chamado pela M2-E1-T15); sem bandeja, fechar encerra a GUI (o daemon continua). `tauri-plugin-single-instance` é o primeiro plugin: uma segunda execução entrega os argumentos à primeira e sai 0; a primeira restaura, mostra e foca a janela (ou espera o `ready` se ainda está abrindo); uma segunda execução com `--minimized` (autostart) não faz nada.

## Medição: Acrylic × opaco no Windows 10
Windows 10 Enterprise 19045, sessão **RDP** (a deste ambiente), release `Forgebox.exe`, `FORGE_GUI_EFFECT=none` (janela opaca) × `=acrylic` (transparente + `apply_acrylic`), com a máquina compilando outros worktrees. `apps/desktop/scripts/window-qa.mjs --checks pacing` move (arrasto: senoide de ±120×±60 px) e redimensiona (±160×±100 px) a janela por `SetWindowPos` a cada 16 ms durante 4 s (`scripts/qa/window-probe.ps1`), como o laço modal de mover/redimensionar faz, e mede a latência de cada chamada (a thread de UI responde `WM_WINDOWPOSCHANGING/CHANGED` de forma síncrona) e os intervalos de `requestAnimationFrame` no WebView2 (CDP). Duas rodadas:

| fase | métrica | opaco (1ª / 2ª) | Acrylic (1ª / 2ª) |
|---|---|---|---|
| parado | rAF p50 / p99 / máx (ms) | 33,0 / 33,1 / 33,2 · 33,0 / 33,2 / 33,8 | 33,0 / 33,2 / 34,1 · 33,0 / 33,1 / 33,2 |
| arrasto | `SetWindowPos` p50 / p95 / p99 / máx (ms) | 2,52 / 5,31 / 6,41 / 59,4 · 2,15 / 4,29 / 4,80 / 5,0 | 2,59 / 4,84 / 5,37 / 62,6 · 2,02 / 4,18 / 5,64 / 475,5 |
| arrasto | rAF p99 / máx (ms) | 33,2 / 33,4 · 33,2 / 34,0 | 33,2 / 33,5 · 33,2 / 33,2 |
| redimensionar | `SetWindowPos` p50 / p95 / p99 / máx (ms) | 7,04 / 10,73 / 35,69 / 214,1 · 7,27 / 10,59 / 11,69 / 24,2 | 5,95 / 9,65 / 15,42 / 18,4 · 5,22 / 8,40 / 9,95 / 13,3 |
| redimensionar | rAF p99 / máx (ms) | 33,5 / 230,5 · 33,4 / 35,3 | 33,2 / 33,3 · 33,1 / 33,1 |

Leitura: no RDP o quadro é limitado a ~30 Hz (rAF de 33 ms; a tela virtual anuncia 32 Hz) e os contadores de composição do DWM (`DwmGetCompositionTimingInfo`) não andam — o RDP não compõe o desfoque como o console. Do lado do app (thread de UI e renderização do WebView2) **não há diferença mensurável**; os picos isolados aparecem nos dois lados e acompanham a carga da máquina. O atraso conhecido do Acrylic no Windows 10 (a janela "fica para trás" do cursor) é do DWM no console com GPU e não é observável nesta sessão. **Padrão escolhido: Windows 10 opaco**, Acrylic por opt-in (`win10_acrylic`), como a decisão do plano prevê quando a degradação não pode ser descartada; a medição no console (mesmo script, `--checks pacing`, e a avaliação visual do arrasto) entra na verificação humana do gate M2. Se lá o Acrylic não degradar, trocar `WIN10_DEFAULT` é uma linha e uma ADR nova.

## Alternativas descartadas
- *`set_effects` do Tauri:* `Ok` sem efeito no Windows 10; não serve para decidir o fallback.
- *Win32 próprio no `src-tauri` (hit-test do maximizar, `SetLayeredWindowAttributes`, `DwmSetWindowAttribute`):* quebra a fronteira do CLAUDE.md; os fatos do SO já vêm do forge-core e o resto o Tauri/`window-vibrancy` fazem.
- *Acrylic por padrão no Windows 10:* atrasa arrastar/redimensionar (medição acima) — fica como opt-in.
- *Janela sempre transparente (receita do spike):* quadro preto na abertura sem material (§3).
- *`tauri-plugin-window-state`:* escreve em `%APPDATA%\dev.forgebox.app` e não guarda por monitor (§7).
- *`tauri-plugin-decorum` para snap layouts:* titlebar injetada e `Win+Z` sintético (§6).
- *Tooltip Radix nos botões da janela:* os botões de legenda do Windows usam o tooltip do sistema; o `title` reproduz isso sem dependência.
- *Sobrescrever `--accent` com a cor do Windows agora:* sem a escolha de tons por tema e o `--on-accent` da M2-E4-T6 o contraste quebra em destaques claros; o shell publica `--system-accent` e a M2-E4-T6 decide como usá-lo.

## Consequências
- Telas novas não tocam em `data-theme`/`data-vibrancy`: o shell é a única fonte. Quem substituir o `App` (M2-E1-T10) mantém o `Titlebar` e o `useFirstFrameReady()` no primeiro componente montado.
- A bandeja (M2-E1-T15) chama `set_tray_available(true)` ao criar o ícone; o autostart (M2-E1-T18) passa `--minimized`; a paleta (M2-E1-T14) preenche `searchSlot`/`onOpenPalette`.
- Uma instalação portátil e uma instalada não abrem a GUI ao mesmo tempo (a instância única é pelo `identifier` `dev.forgebox.app`).
- `--perf-startup` com outra GUI aberta só foca a existente e não imprime nada (a medição de partida a frio exige nenhuma GUI aberta, ADR-0036).
- O `prefers-reduced-transparency` do WebView2 reflete o mesmo "Efeitos de transparência" lido pelo snapshot; não há segunda fonte.
- Mica, snap layouts e o Acrylic real no console (fora do RDP) ficam na verificação humana do gate M2.
