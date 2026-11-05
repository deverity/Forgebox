// The app updater and autostart as the webview sees them (ADR-0038). The Rust side owns the
// state machine (`updater.rs`); this file only types its commands and its `forge://updater`
// event. Outside Tauri (vite preview, vitest, Playwright with the fake bridge) the API is
// inert: the updater reports "inactive", which is also what the 0.2.0 release build says.
import { isTauri } from "../../app/appearance";

export const UPDATER_EVENT = "forge://updater";
export const CHANNEL_PREF = "update_channel";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "up_to_date"
  | "available"
  | "downloading"
  | "stopping"
  | "installing"
  | "error";

export type UpdateChannel = "stable" | "beta";

export interface UpdateInfo {
  version: string;
  /** Changelog (markdown), shown before applying. */
  notes: string | null;
  date: string | null;
}

export interface UpdaterError {
  code: string;
  reason: string;
  /** Localized by the shell in the current language. */
  message: string;
}

export interface UpdaterSnapshot {
  /** False in release builds without a production key and endpoint (0.2.0). */
  active: boolean;
  current_version: string;
  phase: UpdatePhase;
  available: UpdateInfo | null;
  last_checked_ms: number | null;
  error: UpdaterError | null;
}

export interface AutostartStatus {
  available: boolean;
  enabled: boolean;
  /** Localized reason when unavailable (portable mode). */
  reason: string | null;
}

export interface UpdaterApi {
  status: () => Promise<UpdaterSnapshot>;
  check: () => Promise<UpdaterSnapshot>;
  /** Downloads, verifies, stops the daemon and runs the installer; the app restarts. */
  apply: () => Promise<void>;
  /** Calls `onChange` for every state change; resolves to the unsubscribe function. */
  subscribe: (onChange: (snapshot: UpdaterSnapshot) => void) => Promise<() => void>;
  channel: () => Promise<UpdateChannel>;
  setChannel: (channel: UpdateChannel) => Promise<void>;
  autostart: () => Promise<AutostartStatus>;
  setAutostart: (enabled: boolean) => Promise<AutostartStatus>;
}

export const INACTIVE_SNAPSHOT: UpdaterSnapshot = {
  active: false,
  current_version: "",
  phase: "idle",
  available: null,
  last_checked_ms: null,
  error: null,
};

/** Inert API: inactive updater, autostart unavailable. */
export const inertUpdaterApi: UpdaterApi = {
  status: () => Promise.resolve(INACTIVE_SNAPSHOT),
  check: () => Promise.resolve(INACTIVE_SNAPSHOT),
  apply: () => Promise.resolve(),
  subscribe: () => Promise.resolve(() => undefined),
  channel: () => Promise.resolve("stable"),
  setChannel: () => Promise.resolve(),
  autostart: () => Promise.resolve({ available: false, enabled: false, reason: null }),
  setAutostart: () => Promise.resolve({ available: false, enabled: false, reason: null }),
};

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke: call } = await import("@tauri-apps/api/core");
  return call<T>(command, args);
}

export const tauriUpdaterApi: UpdaterApi = {
  status: () => invoke<UpdaterSnapshot>("updater_status"),
  check: () => invoke<UpdaterSnapshot>("updater_check"),
  apply: () => invoke<undefined>("updater_apply"),
  async subscribe(onChange) {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<UpdaterSnapshot>(UPDATER_EVENT, (event) => {
      onChange(event.payload);
    });
  },
  async channel() {
    const prefs = await invoke<Record<string, unknown>>("gui_prefs_get");
    return prefs[CHANNEL_PREF] === "beta" ? "beta" : "stable";
  },
  async setChannel(channel) {
    await invoke("gui_prefs_set", { patch: { [CHANNEL_PREF]: channel } });
  },
  autostart: () => invoke<AutostartStatus>("autostart_get"),
  setAutostart: (enabled) => invoke<AutostartStatus>("autostart_set", { enabled }),
};

export function defaultUpdaterApi(): UpdaterApi {
  return isTauri() ? tauriUpdaterApi : inertUpdaterApi;
}
