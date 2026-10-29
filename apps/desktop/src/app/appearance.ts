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
  }
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
