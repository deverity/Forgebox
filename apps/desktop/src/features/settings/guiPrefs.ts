// GUI-local preferences (theme, close to tray): they live in `prefs.json` of the desktop app
// (`gui_prefs_get/set`, ADR-0034 §5) and never reach the daemon (`system.config_set` is for
// `forgebox.toml`). Outside Tauri (QA builds, tests) they fall back to localStorage.
import { isTauri } from "../../app/appearance";

export type ThemePref = "system" | "light" | "dark";
export const THEME_PREFS: readonly ThemePref[] = ["system", "light", "dark"];

export interface GuiPrefsValues {
  theme: ThemePref;
  close_to_tray: boolean;
}

export const GUI_PREF_DEFAULTS: GuiPrefsValues = { theme: "system", close_to_tray: true };

const LOCAL_KEY = "forgebox.prefs";

export interface GuiPrefsApi {
  get(): Promise<GuiPrefsValues>;
  set(patch: Partial<GuiPrefsValues>): Promise<void>;
}

function normalize(raw: unknown): GuiPrefsValues {
  const doc = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {};
  const theme = THEME_PREFS.find((t) => t === doc.theme) ?? GUI_PREF_DEFAULTS.theme;
  const closeToTray =
    typeof doc.close_to_tray === "boolean" ? doc.close_to_tray : GUI_PREF_DEFAULTS.close_to_tray;
  return { theme, close_to_tray: closeToTray };
}

const tauriPrefs: GuiPrefsApi = {
  async get() {
    const { invoke } = await import("@tauri-apps/api/core");
    return normalize(await invoke("gui_prefs_get"));
  },
  async set(patch) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("gui_prefs_set", { patch });
  },
};

function readLocal(): Record<string, unknown> {
  try {
    const text = globalThis.localStorage.getItem(LOCAL_KEY);
    return text === null ? {} : (JSON.parse(text) as Record<string, unknown>);
  } catch {
    return {};
  }
}

const localPrefs: GuiPrefsApi = {
  get: () => Promise.resolve(normalize(readLocal())),
  set(patch) {
    globalThis.localStorage.setItem(LOCAL_KEY, JSON.stringify({ ...readLocal(), ...patch }));
    return Promise.resolve();
  },
};

export function defaultGuiPrefs(): GuiPrefsApi {
  return isTauri() ? tauriPrefs : localPrefs;
}
