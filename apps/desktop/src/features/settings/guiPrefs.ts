// GUI-local preferences (theme, close to tray): they live in `prefs.json` of the desktop app
// (`gui_prefs_get/set`, ADR-0034 §5) and never reach the daemon (`system.config_set` is for
// `forgebox.toml`). Outside Tauri (QA builds, tests) they fall back to localStorage.
import { isTauri } from "../../app/appearance";

export type ThemePref = "system" | "light" | "dark";
export const THEME_PREFS: readonly ThemePref[] = ["system", "light", "dark"];

export type LanguagePref = "pt-BR" | "en-US";
export const LANGUAGE_PREFS: readonly LanguagePref[] = ["pt-BR", "en-US"];

export interface GuiPrefsValues {
  theme: ThemePref;
  close_to_tray: boolean;
  /** Chosen UI language; `null` follows the OS locale (read at boot by `main.tsx`, E2-02). */
  language: LanguagePref | null;
}

export const GUI_PREF_DEFAULTS: GuiPrefsValues = {
  theme: "system",
  close_to_tray: true,
  language: null,
};

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
  const language = LANGUAGE_PREFS.find((l) => l === doc.language) ?? null;
  return { theme, close_to_tray: closeToTray, language };
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

/**
 * The saved UI language for the boot path: `null` when none is saved, the read fails or takes
 * longer than `timeoutMs` (first paint never waits on it).
 */
export async function readSavedLanguage(
  api: GuiPrefsApi = defaultGuiPrefs(),
  timeoutMs = 250,
): Promise<LanguagePref | null> {
  const timeout = new Promise<null>((resolve) => {
    setTimeout(() => {
      resolve(null);
    }, timeoutMs);
  });
  try {
    const prefs = await Promise.race([api.get(), timeout]);
    return prefs?.language ?? null;
  } catch {
    return null;
  }
}

/** Persists the language choice (best effort: a failure is logged, the switch already happened). */
export async function saveLanguage(
  language: LanguagePref,
  api: GuiPrefsApi = defaultGuiPrefs(),
): Promise<void> {
  try {
    await api.set({ language });
  } catch (error) {
    console.warn("could not save the language preference", error);
  }
}
