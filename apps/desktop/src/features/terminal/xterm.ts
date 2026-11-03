// The xterm.js emulator of one tab (docs/07 §5.10): fit, web-links, search and unicode11
// addons, the WebGL renderer with an automatic fallback to the DOM renderer (WebGL can be
// missing or slow under RDP; a lost context also falls back), fonts and colors from the design
// tokens. Everything is imported lazily: the Terminal screen is a route chunk of its own.
import type { Terminal, ITheme } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import type { SearchAddon } from "@xterm/addon-search";
import { openUrl } from "../../ipc/native";

export type Renderer = "webgl" | "dom";

export interface XtermBundle {
  term: Terminal;
  fit: FitAddon;
  search: SearchAddon;
  renderer: Renderer;
  /** Re-reads the tokens (theme change) and applies them. */
  applyTheme(): void;
  dispose(): void;
}

export const FONT_SIZE = 13;
export const SCROLLBACK = 5000;

/** Reads a CSS custom property of the root; `undefined` when unset (xterm keeps its default). */
function token(name: string): string | undefined {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value === "" ? undefined : value;
}

const HEX6 = /^#[0-9a-f]{6}$/i;

/** Terminal colors from the tokens (`--bg-surface`, `--fg-primary`, `--accent`, `--state-*`). */
export function themeFromTokens(): ITheme {
  const accent = token("--accent");
  const theme: ITheme = {};
  const put = (key: Exclude<keyof ITheme, "extendedAnsi">, value: string | undefined) => {
    if (value !== undefined) theme[key] = value;
  };
  put("background", token("--bg-surface"));
  put("foreground", token("--fg-primary"));
  put("cursor", accent);
  put("cursorAccent", token("--bg-surface"));
  // 33% alpha over the surface; only a #RRGGBB accent can take the alpha suffix.
  if (accent !== undefined && HEX6.test(accent)) theme.selectionBackground = `${accent}55`;
  put("red", token("--state-failed"));
  put("green", token("--state-running"));
  put("yellow", token("--state-starting"));
  put("blue", accent);
  put("cyan", token("--state-updating"));
  put("brightBlack", token("--fg-tertiary"));
  return theme;
}

/** `--font-mono` (Cascadia Code, then the bundled JetBrains Mono). */
export function monoFontFamily(): string {
  return token("--font-mono") ?? "monospace";
}

async function loadMono(): Promise<void> {
  try {
    // The bundled font is a local file: this never reaches the network.
    await document.fonts.load(`${String(FONT_SIZE)}px "JetBrains Mono"`);
  } catch {
    /* the stack falls back to the next monospace */
  }
}

/** Creates the emulator inside `element` (already attached to the document). */
export async function createXterm(element: HTMLElement): Promise<XtermBundle> {
  const [xterm, fitMod, linksMod, searchMod, unicodeMod] = await Promise.all([
    import("@xterm/xterm"),
    import("@xterm/addon-fit"),
    import("@xterm/addon-web-links"),
    import("@xterm/addon-search"),
    import("@xterm/addon-unicode11"),
    import("@xterm/xterm/css/xterm.css"),
    loadMono(),
  ]);
  const term = new xterm.Terminal({
    fontFamily: monoFontFamily(),
    fontSize: FONT_SIZE,
    cursorBlink: true,
    scrollback: SCROLLBACK,
    allowProposedApi: true,
    theme: themeFromTokens(),
  });
  const fit = new fitMod.FitAddon();
  const search = new searchMod.SearchAddon();
  term.loadAddon(fit);
  term.loadAddon(search);
  term.loadAddon(
    new linksMod.WebLinksAddon((_event, uri) => {
      void openUrl(uri);
    }),
  );
  term.loadAddon(new unicodeMod.Unicode11Addon());
  term.unicode.activeVersion = "11";
  term.open(element);

  let renderer: Renderer = "dom";
  let webgl: { dispose(): void } | null = null;
  try {
    const mod = await import("@xterm/addon-webgl");
    const addon = new mod.WebglAddon();
    // A lost context (GPU reset, RDP session switch) drops back to the DOM renderer.
    addon.onContextLoss(() => {
      addon.dispose();
      webgl = null;
    });
    term.loadAddon(addon);
    webgl = addon;
    renderer = "webgl";
  } catch {
    renderer = "dom";
  }

  return {
    term,
    fit,
    search,
    renderer,
    applyTheme() {
      term.options.theme = themeFromTokens();
    },
    dispose() {
      webgl?.dispose();
      term.dispose();
    },
  };
}
