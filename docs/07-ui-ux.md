# 07 — UI/UX e design system ("Apple-ish")

Meta: o Forgebox deve parecer um app de primeira linha do macOS rodando no Windows, sem parecer um site. Isso se obtém com **disciplina**, não com efeitos: hierarquia tipográfica rígida, grid de 4/8 px, poucas cores, materiais translúcidos usados com propósito, motion com física e estados sempre explicados.

Este documento é normativo para `apps/desktop/`. Exemplos de telas (wireframes) ficam em `docs/design/`.

## 1. Princípios
1. **Clareza antes de densidade.** Cada tela responde a uma pergunta: "o que está rodando?", "o que está errado?", "o que posso fazer agora?".
2. **Estado sempre visível e explicável.** Nenhum spinner sem texto. Nenhum erro sem causa + ação sugerida ("Porta 3306 em uso por mysqld.exe (XAMPP). Encerrar | Trocar porta").
3. **Direto, sem modais desnecessários.** Edição inline, popovers, sheets laterais. Modal só para destrutivo.
4. **Teclado em primeiro lugar.** Command palette (`Ctrl+K`), atalhos globais, navegação por Tab com foco visível.
5. **Motion com significado.** Animações informam origem/destino; springs (não ease-in-out lineares); respeitam `prefers-reduced-motion`.
6. **Nativo onde importa.** Titlebar custom integrada, Mica/Acrylic, menu de contexto nativo, notificações do Windows, snap layouts funcionando (Aero Snap e `Win+setas` já; o menu do Windows 11 no hover do maximizar fica para o gate M2, [ADR-0035](adr/0035-window-chrome-materials.md) §6), DPI por monitor.
7. **Paridade GUI = CLI.** Toda ação mostra o comando `forge` equivalente no tooltip/rodapé ("Copiar comando"). O texto vem do `cli_template` da linha do método em `methods.ts` ([ADR-0037](adr/0037-cli-equivalent-templates.md)), nunca de string escrita à mão na tela.

## 2. Fundamentos visuais

### 2.1 Tipografia
- Família: `"SF Pro Text"` se disponível → `"Segoe UI Variable Text"` (Win11) → `"Segoe UI"` → `Inter` (embutida no bundle via `@fontsource`, latin + latin-ext; ver docs/08 §2.5). Monoespaçada: `"Cascadia Code"` → `"JetBrains Mono"` (embutida).
- Escala (px / line-height / weight): Display 28/34/600 · Title1 22/28/600 · Title2 17/22/600 · Headline 15/20/600 · Body 13/18/400 · Callout 12/16/400 · Caption 11/14/400 · Mono 12/18.
- `font-feature-settings: "tnum"` em números (portas, métricas) para alinhamento.
- Nunca texto abaixo de 11 px. Nunca justificado. Nunca caixa alta para ênfase (exceto labels de seção com tracking +4%).

### 2.2 Espaçamento e raio
- Grid 4 px; espaçamentos oficiais: 4, 8, 12, 16, 20, 24, 32, 40.
- Raios: controles 6 px; cards 10 px; sheets/popovers 12 px; janela 8 px (Win11 cuida).
- Bordas 1 px com cor `--stroke` (alpha), nunca cinza sólido.

### 2.3 Cor (tokens em `src/design/tokens.css`)
Fonte única: [`docs/design/tokens.json`](design/tokens.json) (DTCG). `pnpm -C apps/desktop tokens` gera `tokens.css` (temas, `data-vibrancy`, `data-contrast="high"`, `forced-colors`, `@theme` do Tailwind 4) e `tokens.ts`; `pnpm tokens:check` falha em CI se houver drift. Valores light dos estados são mais escuros que os abaixo (dark) para cumprir 3:1. `--stroke` é divisor decorativo (alpha 10%, isento de 3:1); bordas de controles usam `--stroke-strong` (≥ 3:1). Lint: sem literais de cor (ESLint `no-restricted-syntax`, stylelint `color-no-hex`) fora de `tokens.css`/`tokens.ts`, sem fonte < 11 px.

```
--bg-canvas        base da janela (transparente quando vibrancy ativa; sólido no fallback)
--bg-sidebar       material lateral (Acrylic) / sólido
--bg-surface       cards, listas            --bg-surface-2  surfaces elevadas
--fg-primary       texto principal          --fg-secondary  texto secundário  --fg-tertiary labels
--fg-on-accent     texto/ícone sobre fundo --accent (4,5:1; branco no light, preto no dark e alto contraste)
--stroke           divisores (alpha 8–12%)
--accent           cor do sistema Windows (`SystemAppearance.accent` do forge-core; o `AccentColor` do CSS não é confiável no WebView2) com fallback #0A84FF
--state-running    #30D158   --state-starting #FFD60A   --state-stopped #8E8E93   --state-failed #FF453A   --state-updating #64D2FF
--tool-*           cor por tool do manifesto (ui.color), usada só em ícone/acento, nunca em fundo grande
```
- Light e dark completos. Dark é o padrão quando o sistema é dark. Contraste mínimo WCAG AA (4.5:1 texto, 3:1 UI).
- **Materiais** ([ADR-0035](adr/0035-window-chrome-materials.md)): a janela inteira recebe o material e o CSS decide onde ele aparece — sidebar e titlebar sobre o material (`--bg-canvas`/`--bg-sidebar` transparentes com `data-vibrancy="on"`), conteúdo principal **opaco** (token sólido) para legibilidade e performance. Política (`choose_effect`, função pura em `src-tauri/src/window.rs`), nesta ordem: o usuário desligou (`vibrancy: "off"` nas preferências da GUI), alto contraste, sessão RDP ou "Efeitos de transparência" desligado → **opaco**; Windows 11 (build ≥ 22000) → **Mica**; Windows 10 1809+ → **opaco por padrão**, Acrylic só com o opt-in `win10_acrylic` (medido no M2-E1-T7: o Acrylic do Windows 10 atrasa arrastar/redimensionar, ver a ADR); mais antigo/desconhecido → opaco. O efeito é aplicado pelo crate `window-vibrancy` e o `Result` dele decide o fallback (o `set_effects` do Tauri responde `Ok` no Windows 10 sem fazer nada e não é usado). Reavaliado quando o tema muda, quando a janela ganha foco (cobre transparência, alto contraste e reconexão RDP) e quando uma preferência muda. O shell põe em `<html>` `data-theme` (`light`/`dark`), `data-vibrancy` (`on` só com material em efeito; `off` pinta `--bg-*-solid` — sem material a área transparente ficaria preta), `data-effect` (`mica`/`acrylic`/`none`), `data-contrast="high"` e `--system-accent` (`src/app/appearance.ts`, antes do primeiro paint por um script de inicialização e depois pelo evento `forge://appearance`). Diagnóstico: `FORGE_GUI_EFFECT=mica|acrylic|none` força a tentativa (QA/medição). Os fatos do SO vêm de `forge_core::platform::SystemAppearance` (via `forge_ipc::base::platform`): `os_build`/`is_win11`, `remote_session` (RDP), `transparency` ("Efeitos de transparência"; o `prefers-reduced-transparency` do WebView2 reflete o mesmo interruptor), `high_contrast`, e `apps_light`/`taskbar_light` (tema dos apps e da barra de tarefas, que escolhe a variante dos ícones da bandeja, §7).

### 2.4 Iconografia
- Set único (Lucide, stroke 1.5) para UI; logos das tools em SVG monocromático + colorido (do registry).
- Tamanhos: 16 (inline), 20 (lista), 24 (toolbar), 32/48 (cards).
- **Ícone do app (marca)** — fonte em [`docs/design/brand/`](design/brand/README.md), a partir da arte de referência do usuário (`reference-icon-tray.png`): caixa escura aberta com o prompt `>_`, camadas roxo/verde e laranja e tampa de vidro azul flutuante. Master vetorial `forgebox-icon.svg` (transparente, sem glow, sem halo em fundo claro) para 40/48/64/256 px e o conjunto do Tauri; 16/20/24/32 px são grades desenhadas pixel a pixel (menos camadas, prompt de 2 px, alinhadas à grade 2:1), não reduções. `apps/desktop/src-tauri/icons/icon.ico` (16, 20, 24, 32, 40, 48, 64, 256) é o recurso 32512 do `Forgebox.exe` (ícone do EXE, da janela e da barra de tarefas, e do futuro instalador) e é embutido com o mesmo id em `forge.exe`, `forgeboxd.exe`, `forgebox-helper.exe`, `forge-mcp.exe` e `forgebox-ctrl.exe` pelos `build.rs` (rc.exe do Windows SDK). Tudo é gerado por `pnpm -C apps/desktop icons`; `icons:check` falha se um arquivo divergir das fontes.

### 2.5 Elevação e profundidade
- Sombras só em popovers/sheets/toasts: `0 8px 24px rgba(0,0,0,.18)` dark / `.08` light. Cards não têm sombra, têm borda + surface.

## 3. Motion
- Biblioteca: Motion (framer-motion) com springs: `stiffness 380, damping 32` para layout; `260/26` para popovers; `500/40` para toggles.
- Durações máximas: micro 120 ms, transições 220 ms, sheets 280 ms.
- Transições compartilhadas (layoutId) entre lista → detalhe de serviço.
- Estados de serviço animam: ponto de status pulsa em `Starting`, "respira" em `Running` (sutil, 2 s), fica estático em `Stopped`, treme 1× em `Failed`.
- Progresso de download: barra com velocidade e ETA; ao concluir, morph para check.
- `prefers-reduced-motion`: tudo vira fade 100 ms.

## 4. Estrutura da janela

```
┌─ titlebar custom (drag) ─────────────────────────────── ⌕ Ctrl+K ─ ○ ─ □ ─ ✕ ┐
│ ┌ Sidebar (Acrylic, 220 px) ┐ ┌ Conteúdo (opaco) ───────────────────────────┐ │
│ │ ● Forgebox   [▶ Start all] │ │ Header da seção (título, filtros, ações)     │ │
│ │                            │ │                                              │ │
│ │ Dashboard                  │ │ ...                                          │ │
│ │ Serviços          3 ● 1 ○  │ │                                              │ │
│ │ Sites                  12  │ │                                              │ │
│ │ PHP                        │ │                                              │ │
│ │ Bancos de dados            │ │                                              │ │
│ │ Mail                    2● │ │                                              │ │
│ │ Logs                       │ │                                              │ │
│ │ Catálogo             ↑ 3   │ │                                              │ │
│ │ Terminal                   │ │                                              │ │
│ │ ──────────────             │ │                                              │ │
│ │ Configurações              │ │                                              │ │
│ │ v1.0.0 · atualizado        │ │                                              │ │
│ └────────────────────────────┘ └──────────────────────────────────────────────┘ │
│ Status bar: daemon ● conectado · CPU 2% · RAM 410 MB · DNS ok · CA ok          │
└───────────────────────────────────────────────────────────────────────────────┘
```
- Barra de status: **CPU/RAM = o próprio daemon** (`daemon.status.cpu_pct`/`rss_bytes`, API 1.1) **+ a stack** (CPU das instâncias; RAM de instâncias e pools PHP), do mesmo poll de 2 s das sparklines (`useStatusTotals`; só com a janela visível e focada, [ADR-0034](adr/0034-desktop-shell-architecture.md), adendo M2-E1-T9). Sem amostra (janela fora de foco desde o início, daemon desconectado) mostra o último valor ou nada, nunca zero inventado.
- **Janela** ([ADR-0035](adr/0035-window-chrome-materials.md), `src-tauri/src/window.rs`): sem decoração do Windows, transparente só quando um material é esperado na abertura (senão opaca com a cor `bg-canvas`, que evita o quadro preto antes de o WebView2 apresentar; um material que fique possível depois vale na próxima abertura), com a sombra do sistema e a borda de redimensionar invisível do Tauri (8 px fora da área visível nas laterais/base; no topo, os primeiros pixels da titlebar redimensionam). Criada **escondida**; o webview chama o comando `ready` depois de um duplo `requestAnimationFrame` que segue o primeiro commit do shell e só então ela aparece (sem flash branco; `--minimized` mantém escondida na bandeja; um vigia mostra a janela após 5 s se `ready` não vier).
- **Titlebar** (`src/app/shell/Titlebar.tsx`, 32 px): região de arrasto em tudo que não é controle (`data-tauri-drag-region="deep"`), duplo clique maximiza/restaura, slot da busca `Ctrl+K` (a paleta da M2-E1-T14 substitui) e `WindowControls` (46×32 como os botões do Windows: minimizar, maximizar/restaurar, fechar com hover vermelho; `aria-label` + tooltip nativo `title`, alcançáveis por Tab). Snap ao arrastar para a borda e `Win+setas` funcionam; o menu de *snap layouts* do Windows 11 no hover do maximizar fica para o gate (precisa de Win11 e, sem Win32 próprio, de suporte do WebView2/plugin; ver a ADR).
- Tamanho mínimo **min(960×600, área de trabalho do monitor)**, recalculado ao trocar de monitor/DPI; sidebar colapsa para ícones abaixo de 1100 px.
- Topo da sidebar é o **seletor de contexto** (Local · remotos). Contexto remoto muda a cor de destaque da janela e mostra latência/conexão ([13](13-remote-mode.md)).
- Janela lembra posição/tamanho/maximizada **por monitor** (`window-state.json` nos dados da GUI; volta encaixada na área de trabalho; com o monitor ausente usa o do outro monitor conhecido, senão centraliza 1200×800). Fecha → esconde na bandeja quando `close_to_tray` (padrão ligado) **e** a bandeja existe (M2-E1-T15); sem bandeja, fechar encerra a GUI (o daemon continua).
- **Instância única**: abrir o `Forgebox.exe` de novo traz a janela existente para a frente (restaura se minimizada); com `--minimized` a segunda execução não faz nada.
- **Dados da GUI** (preferências `prefs.json`, `window-state.json`, perfil do WebView2 em `webview/`): `%LOCALAPPDATA%\Forgebox\gui`, ou `<raiz>\state\gui` no modo portátil, ou `FORGE_GUI_DATA_DIR` (testes/QA). Nunca no `forgebox.toml` ([ADR-0034](adr/0034-desktop-shell-architecture.md) §5). Comandos `gui_prefs_get`/`gui_prefs_set` (chaves conhecidas: `theme`, `vibrancy`, `win10_acrylic`, `close_to_tray`; as demais ficam intactas para as features donas delas).

## 5. Telas

### 5.1 Onboarding (primeira execução, 4 passos, < 2 min)
1. Boas-vindas + escolher pasta raiz (`C:\Forgebox`) e diretório parked (`C:\Forgebox\www`), TLD (`.test`).
2. Escolher stack inicial (cards: Laravel · WordPress · PHP genérico · Node/Next · Vazio) → lista do que será baixado com tamanho total; versões ajustáveis.
3. Permissões e paridade: "Adicionar ao PATH" (toggle), "DNS automático para *.test (precisa de 1 confirmação de admin)" (toggle), "Confiar na CA local" (toggle, explica o diálogo do Windows), "Iniciar com o Windows", **"Modo paridade Linux (case-sensitive)"** ligado por padrão com explicação de uma frase ([15](15-linux-parity-case-sensitivity.md)).
4. Download com progresso por item e dicas; ao final: "Seu primeiro site: arraste uma pasta aqui ou crie um projeto".

### 5.2 Dashboard
- Linha superior: tiles de serviço (Nginx, PHP 8.3 pool, MySQL 8.4, Mailpit…) com status, porta, CPU/RAM sparkline (60 s), botão play/stop, menu `…`.
- "Atenção" (só aparece quando há algo): conflitos de porta, updates disponíveis, cert expirando, drift de config, alertas de desempenho (p99/erros/RAM).
- "Projetos": ranking por CPU/RAM/p99/erros nas últimas 24 h e monitor da máquina (parcela do Forgebox).
- Sites recentes (favicon, host, PHP badge, abrir/terminal/editor).
- Atividade (últimos eventos legíveis: "PHP 8.3.37 instalado · há 2 min").

### 5.3 Serviços
- Lista agrupada por categoria; cada linha: ícone, nome + versão, instância, status com texto ("Running · 2h 14m"), portas, recursos, ações.
- Detalhe (sheet lateral 420 px ou tela cheia): Visão geral · Configuração (quick settings nativos + botão "Abrir arquivo") · Logs (stream, filtro, pausar, copiar) · Instâncias (criar nova: nome, porta, data dir) · Versões (instaladas, usar, remover, rollback) · Avançado (args, env, política de restart).
- Ação destrutiva (remover instância com dados) exige digitar o nome.

### 5.4 Sites
- Grid/lista com busca instantânea; card: favicon + screenshot miniatura (gerada pelo WebView2 offscreen), host (clique abre), badges (PHP 8.3 · Nginx · HTTPS · Laravel · `Aa` case-sensitive), estado do process `web` se houver.
- Criar site: sheet com drag-and-drop de pasta, ou "Novo projeto" (Stacks), ou "Proxy".
- Detalhe: Geral (host, aliases, root, driver detectado com override, servidor, **PHP dropdown com troca ao vivo**, HTTPS toggle) · **Desempenho** (req/s, p50/p90/p99 em bandas, erros, CPU/RAM atribuídos, rotas lentas, consultas lentas, marcadores de eventos; ver [14](14-observability-metrics.md)) · Processos (Procfile UI, start/stop/logs) · Ambiente (`.env` viewer com máscara de segredos) · Banco (criar DB, abrir no HeidiSQL/Adminer) · Compartilhar (túnel + QR) · Diagnóstico (DNS, cert, permissões, `php -v` do pool).

### 5.5 PHP
- Colunas por série instalada (7.4 · 8.2 · 8.3 · 8.4 · 8.5): versão exata, pool (workers ativos/ociosos, req/s), sites que usam, "CLI padrão" (radio), quick settings (memory, upload, exec time, display_errors, opcache), extensões (toggles com busca), Xdebug (modo, porta, IDE key), atualização (política por série, "verificar agora", histórico/rollback).
- "Instalar outra versão" abre o catálogo filtrado com variantes explicadas.

### 5.6 Bancos de dados
- Instâncias (MySQL/MariaDB/PostgreSQL/Mongo) com porta, tamanho em disco, bancos (lista com criar/dropar/exportar/importar com progresso), usuários, string de conexão (copiar: DSN, URL, `.env` snippet), abrir em (HeidiSQL, DBeaver, TablePlus, Adminer, phpMyAdmin, pgAdmin), backups agendados.
- Redis-compatíveis: chaves, memória, `FLUSHALL` com confirmação, abrir RedisInsight.
- No M2 (M2-E3-T14, [ADR-0040](adr/0040-e3-api-terminal-db-mail-logs.md)): instâncias de `db.list.instances` (as que declaram `spec.database`), bancos com criar/dropar/exportar/importar, strings de conexão e shell (`db.shell` num terminal integrado). "Abrir em" (`db.open`), usuários, backups agendados e a seção Redis ficam para o M3 (M3-E1).

### 5.7 Mail
- Inbox do Mailpit renderizada nativamente (lista + preview HTML/texto/headers/anexos, busca, marcar lido, apagar), credenciais SMTP com "copiar `.env`". Badge de não lidos na sidebar e na bandeja.

### 5.8 Logs
- Visualizador unificado: fontes (daemon, nginx access/error, apache, php error, mysql, processes), filtro por texto/nível/site/tempo, follow, pausa, destacar, abrir arquivo, copiar seleção, links `file:line` que abrem no editor configurado (VS Code/PhpStorm) para stack traces PHP.

### 5.9 Catálogo
- Busca + filtros (categoria, instalado, atualização disponível); cards com ícone, nome, descrição curta, versão mais recente, tamanho, publisher/assinatura, badge "nativo Windows" / "via WSL (v2)".
- Página da tool: descrição Markdown, versões (série → patches), variantes (NTS/TS explicado), changelog upstream (link), botão **Instalar** com escolha de versão, política de update. Stacks como cards grandes.

### 5.10 Terminal
- Terminal integrado (xterm.js + ConPTY via Rust) com PATH do Forgebox; abas por site ("Terminal em loja.test", PHP/Node certos); comando `forge` autocomplete.

### 5.11 Configurações
- Geral (pasta raiz, idioma, tema, iniciar com Windows, fechar para bandeja) · Sistema de arquivos (modo case-sensitive global, estado por diretório, Converter, Case Mismatch Finder) · Rede (TLD, DNS, portas padrão, LAN) · Segurança (CA, helper, exportar CA) · Atualizações (canal, políticas globais, horário silencioso, conexão medida) · Editor/Terminal (VS Code, PhpStorm, Windows Terminal) · Registries · Avançado (templates override, logs do daemon, exportar diagnóstico).

## 6. Componentes (primitives em `src/design/primitives/`)
Button (primary/secondary/ghost/destructive, com loading), IconButton, Toggle, Select (Radix), Combobox, Segmented control, Slider, TextField (com validação inline), PathField (com "Procurar…"), PortField (verifica disponibilidade ao digitar), Badge, StatusDot (5 estados), Card, ListRow, Sheet (lateral), Popover, Menu (contexto), Toast (empilhado, ação de desfazer), Progress (linear/circular), Sparkline, EmptyState (ilustração + ação), Skeleton, CommandPalette, Kbd, Tooltip (com comando `forge`).

## 7. Bandeja (tray)
- Ícone (arte do usuário, [docs/design/brand](design/brand/README.md)) = ícone do app + um selo no canto inferior direito (metade do ícone, anel de recorte de 1 px), sempre diferente também pela forma, nunca só pela cor. Estado agregado: **running** (ponto verde: tudo que está habilitado rodando), **starting** (anel azul girando: 8 quadros, ~8 fps só enquanto inicia, quadro estático com movimento reduzido), **stopped** (quadrado vermelho: nada rodando), **updating** (seta azul: atualização pendente ou job de instalação/atualização), **warning** (triângulo âmbar com `!`: algum serviço falhou ou está em crash-loop enquanto outros rodam), **error** (`!` vermelho em disco: daemon inacessível ou todos os serviços habilitados falharam); `base` (sem selo) antes do primeiro estado. Precedência: error > warning > starting > updating > running > stopped (M2-E1-T15).
- Menu nativo: Abrir Forgebox · Start/Stop all · Serviços (submenu com toggle por instância) · Sites (submenu: abrir no navegador) · PHP CLI (submenu radio) · Mail (n não lidos) · Terminal · Sair (pergunta se para serviços).
- Contexto ([13](13-remote-mode.md)): primeira linha do menu mostra o contexto ativo ("● Local" ou "● hml (Linux)") com submenu para trocar; todas as ações do tray agem sobre o contexto ativo; com remoto selecionado o ícone ganha um pequeno indicador (ponto roxo no canto superior esquerdo, a sobreposição `tray/remote-<light|dark>.ico` composta sobre o ícone do estado) e o tooltip diz o nome do remoto, para nunca parar um serviço na máquina errada.
- Hover mostra tooltip com resumo (serviços rodando/falhos, PHP CLI ativo, contexto).
- Clique esquerdo abre/foca a janela; clique direito abre o menu; clique do meio abre o terminal global.
- Notificações nativas do Windows (toast) para: serviço caiu e foi reiniciado, update instalado, download concluído, alerta de desempenho, túnel pronto (com URL copiável). Agrupadas e com "não mostrar de novo" por tipo.
- O tray pertence ao processo da GUI (`Forgebox.exe`). Se a GUI não estiver aberta, o daemon continua rodando sem tray; `forgeboxd` nunca cria UI. Opção "Iniciar com o Windows" sobe a GUI minimizada na bandeja.
- Implementação: `tray-icon` do Tauri 2 (`apps/desktop/src-tauri/src/tray.rs`), ícones por estado em `apps/desktop/src-tauri/icons/tray/<estado>-<light|dark>.ico` embutidos no `Forgebox.exe` (`include_bytes!`; estados `base`, `running`, `starting-0`…`starting-7`, `stopped`, `updating`, `warning`, `error`, mais a sobreposição `remote`), cada um com 16/20/24/32 px (100/125/150/200 %, escolhido pelo DPI da bandeja) e com variante para barra clara (`-light`) e escura (`-dark`, aro mais claro na caixa) escolhida por `SystemAppearance.taskbar_light`; gerados de `docs/design/brand` por `pnpm -C apps/desktop icons`. Menu reconstruído a partir dos eventos do daemon (não por polling).

## 8. Atalhos
`Ctrl+K` palette · `Ctrl+1..9` seções · `Ctrl+Shift+S` start all · `Ctrl+Shift+X` stop all · `Ctrl+N` novo site · `Ctrl+L` logs · `` Ctrl+` `` terminal · `Ctrl+,` configurações · `Ctrl+Alt+T` terminal global (compatível com Laragon). Todos reconfiguráveis.

## 9. Acessibilidade e i18n
- Navegação completa por teclado; `aria-*` via Radix; foco visível com anel `--accent`; textos alternativos em ícones de status (não só cor).
- pt-BR e en-US no lançamento; strings em `src/i18n/`; formatos de data/número via `Intl`.
- i18next + react-i18next; um namespace por feature em `src/i18n/{pt-BR,en-US}/<ns>.json` (`common`, `shell`, `errors`, `tray`, `notifications`, `palette`, + um por feature), carregados sob demanda, chaves tipadas (`src/i18n/i18next.d.ts`: **todo namespace novo entra em `Resources`**). Interpolação `{var}` (mesma sintaxe no Rust).
- Idioma: preferência do usuário > idioma do SO (`navigator.language`, que no WebView2 segue o idioma do Windows) > pt-BR. A troca em runtime re-renderiza sem reload e chama o comando Tauri `set_language`, que mantém `src-tauri/src/i18n.rs` (bandeja e notificações, `include_str!` dos namespaces `tray` e `notifications`) em sincronia.
- Formatadores (`src/i18n/format.ts`): bytes (`410 MB`), duração (`2h 14m`), relativo (`há 2 min`), percentual, número e data via `Intl`; números na UI usam a classe `tabular-nums` (`TNUM_CLASS`).
- Mensagens da API nunca são traduzidas: a UI localiza a partir de `code` e `details` (namespace `errors`).
- Lint: `eslint-plugin-i18next/no-literal-string` em JSX (`eslint.i18n.js`; allowlist de marcas e comandos `forge`). Teste de paridade (`src/i18n/i18n.test.ts`) cobre toda chave/placeholder de todos os namespaces automaticamente.
- Pseudo-locale `pseudo` (acentos, +40 % de comprimento, placeholders preservados) só para QA: aceito apenas fora do modo `production`.
- Escala de fonte do sistema respeitada; testar em 100/125/150/200%.

## 10. Performance (orçamentos)
- Primeiro frame < 800 ms (da criação do processo ao comando `ready`, que mostra a janela; `Forgebox.exe --perf-startup` imprime `{"first_frame_ms": …}` e sai, o comando `perf_startup` devolve as marcas); interação < 50 ms; 60 fps em listas de 500 itens (virtualização); eventos IPC aplicados em batch por frame; nenhuma re-renderização global por tick de métricas (stores por serviço). Medição, harness e QA visual: [ADR-0036](adr/0036-frontend-test-strategy-cold-start.md) (`pnpm perf:startup`, `scripts/e2e-desktop.ps1`).
- Implementação (M2-E1-T9, `src/state`, adendo da [ADR-0034](adr/0034-desktop-shell-architecture.md)): eventos aplicados uma vez por `requestAnimationFrame` (≤ 2048 por lote; timer de 250 ms com a janela oculta; fila descartada e resync acima de 50 000), um `setState` por store por lote e seletores por entidade — teste de aceite: 1 000 eventos num frame → 1 commit no componente da entidade afetada, 0 renders nos demais. Métricas (sparklines, barra de status) por polling de `services.status` a cada 2 s só com um consumidor montado e a janela visível e focada; anel de 30 amostras (60 s); req/s dos pools pelo delta de `requests_total`. Os eventos `service.metrics` não alimentam a GUI.

## 11. Checklist de revisão de UI (para agentes)
- [ ] Usa tokens, nunca cores literais.
- [ ] Toda ação tem estado loading/sucesso/erro com texto.
- [ ] Erro mostra causa + ação.
- [ ] Funciona em light/dark/vibrancy-off.
- [ ] Acessível por teclado; foco visível.
- [ ] Mostra comando `forge` equivalente.
- [ ] Testado em 125% e 150% DPI.
- [ ] Sem layout shift ao chegar dados (skeleton/altura fixa).
