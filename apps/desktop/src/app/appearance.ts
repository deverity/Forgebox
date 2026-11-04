// Window appearance on <html> (docs/07 section 2.3, ADR-0035). The Rust shell decides the theme
// and the window material (src-tauri/src/window.rs: choose_effect + window-vibrancy's answer)
// and publishes the result three ways:
//   1. `window.__FORGEBOX_SHELL__`, set by an initialization script before any page script, so
//      the attributes are in place before the first paint (the window is still hidden);
//   2. the `forge://appearance` event on every change (theme, focus, preferences);
//   3. the `appearance_get` command, asked once the listener is attached (no lost update).
// Outside Tauri (vite preview, Playwright, vitest) `?theme=light|dark&vibrancy=on|off` stand in.
// Without anything the CSS defaults hold: system theme, opaque.

export const APPEARANCE_EVENT = "forge://appearance";
export const APPEARANCE_COMMAND = "appearance_get";

export type ThemeName = "light" | "dark";
export type VibrancyState = "on" | "off";
export type WindowEffect = "mica" | "acrylic" | "none";

/** `AppearanceState` of src-tauri/src/window.rs. */
export interface ShellAppearance {
  theme: ThemeName;
  /** `on` exactly when a material is in effect; `off` paints the opaque fallback. */
  vibrancy: VibrancyState;
  effect: WindowEffect;
  /** Why (`win11`, `win10_default`, `remote_session`, `transparency_off`, `user_off`...). */
  reason: string;
  contrast: "high" | "normal";
  /** Windows accent `#RRGGBB`, exposed as `--system-accent`. */
  accent: string;
  remote_session: boolean;
  os_build: number;
}

/** What the initialization script publishes. */
export interface ShellBoot {
  appearance: ShellAppearance;
  minimized: boolean;
}

declare global {
  interface Window {
    __FORGEBOX_SHELL__?: ShellBoot;
  }
}

/** The part of the appearance that can be applied (URL params carry only theme/vibrancy). */
export type AppearancePatch = Partial<
  Pick<ShellAppearance, "theme" | "vibrancy" | "effect" | "contrast" | "accent">
>;

const HEX_COLOR = /^#[0-9a-f]{6}$/i;

/** Sets `data-theme`, `data-vibrancy`, `data-effect`, `data-contrast` and `--system-accent`. */
export function applyAppearance(
  appearance: AppearancePatch,
  root: HTMLElement = document.documentElement,
): void {
  if (appearance.theme) root.dataset.theme = appearance.theme;
  if (appearance.vibrancy) root.dataset.vibrancy = appearance.vibrancy;
  if (appearance.effect) root.dataset.effect = appearance.effect;
  if (appearance.contrast === "high") {
    root.dataset.contrast = "high";
  } else if (appearance.contrast === "normal") {
    delete root.dataset.contrast;
  }
  if (appearance.accent && HEX_COLOR.test(appearance.accent)) {
    root.style.setProperty("--system-accent", appearance.accent);
    applyAccent(root, appearance.accent);
  }
  // High contrast keeps its own tokens (they win over any accent).
  if (appearance.contrast === "high") {
    root.style.removeProperty("--accent");
    root.style.removeProperty("--fg-on-accent");
    root.style.removeProperty("--accent-hover");
    root.style.removeProperty("--accent-pressed");
  }
}

/** Relative luminance (WCAG 2.1) of `#RRGGBB`. */
export function luminance(hex: string): number {
  const [r = 0, g = 0, b = 0] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/**
 * Text colour for a fill of `accent`: black or white, whichever has more contrast. The best of
 * the two always reaches 4.5:1 (docs/07 section 2.3), whatever accent the user picked in Windows
 * (white on #FFB900 would be 1.6:1).
 */
export function onAccentColor(accent: string): "black" | "white" {
  const l = luminance(accent);
  return 1.05 / (l + 0.05) >= (l + 0.05) / 0.05 ? "white" : "black";
}

/** Mixes `hex` toward `target` by `amount` (0..1), in sRGB. */
export function mixHex(hex: string, target: "black" | "white", amount: number): string {
  const t = target === "white" ? 255 : 0;
  return (
    "#" +
    [1, 3, 5]
      .map((i) => {
        const c = parseInt(hex.slice(i, i + 2), 16);
        return Math.round(c + (t - c) * amount)
          .toString(16)
          .padStart(2, "0");
      })
      .join("")
  );
}

/**
 * Hover and pressed fills for `accent`: mixed AWAY from the text colour (towards black when the
 * text is white, towards white when it is black), so the contrast with `--fg-on-accent` only
 * grows. Opacity on the button faded the text and the fill (F-09).
 */
export function accentStates(accent: string): { hover: string; pressed: string } {
  const away = onAccentColor(accent) === "white" ? "black" : "white";
  return { hover: mixHex(accent, away, 0.15), pressed: mixHex(accent, away, 0.28) };
}

/** The Windows accent drives `--accent`; `--fg-on-accent` and the hover/pressed fills follow it. */
function applyAccent(root: HTMLElement, accent: string): void {
  if (root.dataset.contrast === "high") return;
  // Forced colours: an inline --accent would beat the Highlight tokens (higher specificity).
  if ("matchMedia" in globalThis && globalThis.matchMedia("(forced-colors: active)").matches)
    return;
  root.style.setProperty("--accent", accent);
  root.style.setProperty("--fg-on-accent", onAccentColor(accent));
  const states = accentStates(accent);
  root.style.setProperty("--accent-hover", states.hover);
  root.style.setProperty("--accent-pressed", states.pressed);
}

function isTheme(value: string | null): value is ThemeName {
  return value === "light" || value === "dark";
}

function isVibrancy(value: string | null): value is VibrancyState {
  return value === "on" || value === "off";
}

/** The appearance known before the first paint: the shell's, else the URL's, else none. */
export function readBootAppearance(win: Window = window): AppearancePatch | null {
  const boot = win.__FORGEBOX_SHELL__;
  if (boot?.appearance) return boot.appearance;
  const params = new URLSearchParams(win.location.search);
  const theme = params.get("theme");
  const vibrancy = params.get("vibrancy");
  const patch: AppearancePatch = {};
  if (isTheme(theme)) patch.theme = theme;
  if (isVibrancy(vibrancy)) patch.vibrancy = vibrancy;
  return Object.keys(patch).length > 0 ? patch : null;
}

/** Whether the page was started with `--minimized` (tray only). */
export function startedMinimized(win: Window = window): boolean {
  return win.__FORGEBOX_SHELL__?.minimized ?? false;
}

/** Applies the boot appearance synchronously; call before the first render. */
export function bootAppearance(win: Window = window): void {
  const patch = readBootAppearance(win);
  if (patch) applyAppearance(patch, win.document.documentElement);
}

/** Where live appearance updates come from (Tauri in the app, a fake in tests). */
export interface AppearanceSource {
  listen(onChange: (appearance: ShellAppearance) => void): Promise<() => void>;
  current(): Promise<ShellAppearance>;
}

export function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in globalThis;
}

/** The Tauri source: the `forge://appearance` event and the `appearance_get` command. */
export const tauriAppearanceSource: AppearanceSource = {
  async listen(onChange) {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<ShellAppearance>(APPEARANCE_EVENT, (event) => {
      onChange(event.payload);
    });
  },
  async current() {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<ShellAppearance>(APPEARANCE_COMMAND);
  },
};

/**
 * Keeps <html> in sync with the shell: listens first, then asks for the current state, so an
 * update between the boot script and the listener is never lost. Never rejects; returns the
 * unsubscribe function (a no-op outside Tauri).
 */
export async function followAppearance(
  source: AppearanceSource | null = isTauri() ? tauriAppearanceSource : null,
  onChange: (appearance: ShellAppearance) => void = (a) => {
    applyAppearance(a);
  },
): Promise<() => void> {
  if (!source) return () => undefined;
  let stop: () => void = () => undefined;
  try {
    stop = await source.listen(onChange);
    onChange(await source.current());
  } catch (error) {
    console.warn("appearance: live updates unavailable", error);
  }
  return stop;
}
