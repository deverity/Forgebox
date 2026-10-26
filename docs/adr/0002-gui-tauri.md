# ADR-0002 — GUI com Tauri 2 + WebView2

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Requisito: UI "Apple-ish" de alta qualidade (translucência, motion, tipografia, componentes acessíveis) com bundle pequeno e baixo uso de memória. Herd prova que Electron entrega visual mas custa ~1,2 GB em disco e centenas de MB de RAM.

## Decisão
Tauri 2 com WebView2 (já presente em Windows 10/11), frontend React 19 + TypeScript + Tailwind 4 + Radix + Motion, efeitos nativos via `window-vibrancy` (Acrylic/Mica), titlebar custom. O código Rust do app Tauri só faz ponte para a API do daemon (nenhuma lógica de domínio).

## Alternativas rejeitadas
- **Electron:** tamanho/memória; duplicaria o runtime Chromium.
- **GUI Rust pura (iced, egui, Slint, gpui, Dioxus native):** egui é imediato e "gamey"; iced/Slint carecem de maturidade em acessibilidade, IME, text rendering fino e ecossistema de componentes para atingir acabamento Apple em prazo razoável; gpui é acoplado ao Zed. Reavaliar em 2027.
- **WinUI 3 / WPF:** limita portabilidade futura e exige C#/C++ ou bindings imaturos.
- **Flutter:** runtime próprio (~30 MB), look não nativo, Dart no stack.

## Consequências
- Dependemos do WebView2 Runtime (Evergreen; instalador NSIS do Tauri faz bootstrap se faltar).
- Conteúdo da janela deve ter painel opaco para Acrylic não degradar performance ao redimensionar; fallback opaco em RDP/VM.
- Tipos IPC gerados (Rust → TS) para evitar drift.
