// The native side of the onboarding (M2-E2-T11 commands + the E1 autostart API) behind one
// interface, so the screen runs unchanged in the app (Tauri commands), in QA builds and in
// vitest (an in-memory fake). The commands carry no domain logic: the root, the progress file
// and the daemon start live in src-tauri/src/onboarding.rs.
import { isTauri } from "../../app/appearance";
import type { ForgeApi } from "../../ipc/api";
import type { ContextStateDto } from "../../ipc/bridge";
import { ForgeApiError } from "../../ipc/errors";
import { defaultUpdaterApi, type AutostartStatus } from "../updater/api";

/** Where the root of this run comes from (`FirstRunState.source`). */
export type RootOrigin = "env" | "portable" | "root_toml" | "default";

/** `first_run_state`. */
export interface FirstRunState {
  context: string;
  local: boolean;
  root: string | null;
  source: RootOrigin | null;
  root_initialized: boolean;
  daemon_reachable: boolean;
  onboarding_done: boolean;
  /** Local, not finished and the root is not forced by an override. */
  show_root_step: boolean;
}

/** The stored progress (`onboarding_get`). */
export interface OnboardingEntry {
  /** Id of the step the user is on (`""`: not started). */
  step: string;
  choices: Record<string, unknown>;
  done: boolean;
}

/** `onboarding_set`: absent fields stay, `choices` merges by key (`null` removes one). */
export interface OnboardingPatch {
  step?: string;
  choices?: Record<string, unknown>;
  done?: boolean;
}

export interface OnboardingNative {
  firstRunState(context: string): Promise<FirstRunState>;
  /** Validates, writes `root.toml` and starts the daemon of the new root (initializing it). */
  chooseRoot(path: string, api: ForgeApi): Promise<{ root: string }>;
  get(context: string): Promise<OnboardingEntry>;
  set(context: string, patch: OnboardingPatch): Promise<OnboardingEntry>;
  autostart(): Promise<AutostartStatus>;
  setAutostart(enabled: boolean): Promise<AutostartStatus>;
}

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke: call } = await import("@tauri-apps/api/core");
  return call<T>(command, args);
}

export const tauriOnboardingNative: OnboardingNative = {
  firstRunState: (context) => invoke<FirstRunState>("first_run_state", { context }),
  async chooseRoot(path) {
    const answer = await invoke<{ root: string; state: ContextStateDto }>("choose_root", { path });
    return { root: answer.root };
  },
  get: (context) => invoke<OnboardingEntry>("onboarding_get", { context }),
  set: (context, patch) => invoke<OnboardingEntry>("onboarding_set", { context, patch }),
  autostart: () => defaultUpdaterApi().autostart(),
  setAutostart: (enabled) => defaultUpdaterApi().setAutostart(enabled),
};

export interface MemoryNativeOptions {
  root?: string;
  source?: RootOrigin;
  local?: boolean;
  autostart?: AutostartStatus;
  entry?: Partial<OnboardingEntry>;
  /** Keep the progress in `sessionStorage` so a page reload resumes (QA builds). */
  persist?: boolean;
  /** `choose_root` refuses with this `details.reason`. */
  rejectRoot?: string;
}

const STORAGE_KEY = "forgebox.onboarding.fake";

/** In-memory stand-in with the semantics of the Tauri commands. */
export function createMemoryNative(options: MemoryNativeOptions = {}): OnboardingNative & {
  /** The root `chooseRoot` accepted last. */
  chosen: string | null;
  autostartEnabled: boolean;
} {
  const source = options.source ?? "default";
  const changeable = source === "default" || source === "root_toml";
  // Like `key_for` in src-tauri/src/onboarding.rs, the progress is keyed by the current root
  // (`chooseRoot` switches the key to the chosen path), so a copy that is missing shows up.
  const initialRoot = options.root ?? "C:/Forgebox";
  let currentRoot = initialRoot;
  const entries = new Map<string, OnboardingEntry>();
  const blank = (): OnboardingEntry => ({ step: "", choices: {}, done: false });
  const fold = (root: string) => root.replaceAll("\\", "/").replace(/\/+$/, "").toLowerCase();
  const entryOf = (): OnboardingEntry => entries.get(fold(currentRoot)) ?? blank();
  entries.set(fold(initialRoot), { ...blank(), ...options.entry });
  if (options.persist === true) {
    try {
      const stored = sessionStorage.getItem(STORAGE_KEY);
      if (stored !== null) {
        const parsed = JSON.parse(stored) as { root: string; entries: [string, OnboardingEntry][] };
        currentRoot = parsed.root;
        entries.clear();
        for (const [key, value] of parsed.entries) entries.set(key, value);
      }
    } catch {
      /* no storage: in-memory only */
    }
  }
  let autostart: AutostartStatus = options.autostart ?? {
    available: true,
    enabled: false,
    reason: null,
  };
  const save = () => {
    if (options.persist !== true) return;
    try {
      sessionStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({ root: currentRoot, entries: [...entries] }),
      );
    } catch {
      /* ignore */
    }
  };
  const fake = {
    chosen: null as string | null,
    get autostartEnabled() {
      return autostart.enabled;
    },
    firstRunState: (context: string): Promise<FirstRunState> =>
      Promise.resolve({
        context,
        local: options.local ?? true,
        root: options.local === false ? null : currentRoot,
        source: options.local === false ? null : source,
        root_initialized: false,
        daemon_reachable: false,
        onboarding_done: entryOf().done,
        show_root_step: (options.local ?? true) && !entryOf().done && changeable,
      }),
    async chooseRoot(path: string, api: ForgeApi) {
      if (options.rejectRoot !== undefined) {
        throw new ForgeApiError({
          code: "invalid_input",
          message: "the folder cannot be the root",
          details: { reason: options.rejectRoot, args: {} },
        });
      }
      fake.chosen = path;
      currentRoot = path;
      await api.start({ initialize: true });
      return { root: path };
    },
    get: (): Promise<OnboardingEntry> => Promise.resolve(structuredClone(entryOf())),
    set: (_context: string, patch: OnboardingPatch): Promise<OnboardingEntry> => {
      const entry = entryOf();
      const merged = { ...entry.choices, ...patch.choices };
      const choices = Object.fromEntries(Object.entries(merged).filter(([, v]) => v !== null));
      const next: OnboardingEntry = {
        step: patch.step ?? entry.step,
        choices,
        done: patch.done ?? entry.done,
      };
      entries.set(fold(currentRoot), next);
      save();
      return Promise.resolve(structuredClone(next));
    },
    autostart: (): Promise<AutostartStatus> => Promise.resolve({ ...autostart }),
    setAutostart: (enabled: boolean): Promise<AutostartStatus> => {
      if (autostart.available) autostart = { ...autostart, enabled };
      return Promise.resolve({ ...autostart });
    },
  };
  return fake;
}

let override: OnboardingNative | null = null;
let qaNative: OnboardingNative | null = null;

/** Tests install their own native; `null` restores the default. */
export function setOnboardingNativeForTest(native: OnboardingNative | null): void {
  override = native;
}

/** QA builds read `?portable=1` / `?root=env` from the URL, like the other scenario switches. */
function qaOptions(): MemoryNativeOptions {
  const params = new URLSearchParams(globalThis.location.search);
  const portable = params.get("portable") === "1";
  return {
    persist: true,
    ...(portable
      ? {
          source: "portable" as const,
          autostart: {
            available: false,
            enabled: false,
            reason: "Portable mode does not touch the Windows startup entries.",
          },
        }
      : {}),
  };
}

/** The native of the running app: Tauri commands, or the in-memory fake outside Tauri. */
export function onboardingNative(): OnboardingNative {
  if (override !== null) return override;
  if (isTauri()) return tauriOnboardingNative;
  qaNative ??= createMemoryNative(qaOptions());
  return qaNative;
}
