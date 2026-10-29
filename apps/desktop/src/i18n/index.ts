import i18next, { type BackendModule, type i18n as I18n } from "i18next";
import { initReactI18next } from "react-i18next";
import { pseudoizeBundle } from "./pseudo";

/** Shipped locales; the QA-only pseudo-locale is never offered to end users. */
export const SUPPORTED_LOCALES = ["pt-BR", "en-US"] as const;
export type Locale = (typeof SUPPORTED_LOCALES)[number];
export type AnyLocale = Locale | "pseudo";
export const DEFAULT_LOCALE: Locale = "pt-BR";
export const PSEUDO_LOCALE = "pseudo";

/** Namespaces every window needs on first paint; the rest load lazily on demand. */
export const CORE_NAMESPACES = ["common", "shell", "errors"] as const;
export const DEFAULT_NAMESPACE = "common";

/** Name of the Tauri command that keeps the Rust side (tray, notifications) in sync. */
export const SET_LANGUAGE_COMMAND = "set_language";

type Bundle = Record<string, unknown>;
// Every namespace JSON under src/i18n/<locale>/ is picked up by the glob.
const loaders = import.meta.glob<Bundle>("./*/*.json", { import: "default" });

async function loadBundle(lng: string, ns: string): Promise<Bundle> {
  const source = lng === PSEUDO_LOCALE ? "en-US" : lng;
  const load = loaders[`./${source}/${ns}.json`];
  if (!load) throw new Error(`i18n: missing namespace ${ns} for ${lng}`);
  const bundle = await load();
  return lng === PSEUDO_LOCALE ? pseudoizeBundle(bundle) : bundle;
}

/** Lazy backend: a namespace's JSON is only fetched when first used (code-split by Vite). */
const lazyBackend: BackendModule = {
  type: "backend",
  init() {
    /* nothing to configure */
  },
  read(language, namespace, callback) {
    loadBundle(language, namespace).then(
      (data) => {
        callback(null, data);
      },
      (error: unknown) => {
        callback(error instanceof Error ? error : new Error(String(error)), null);
      },
    );
  },
};

/** Maps an OS/browser locale tag to a shipped locale; anything unknown is pt-BR. */
export function resolveLocale(...candidates: (string | null | undefined)[]): Locale {
  for (const raw of candidates) {
    if (!raw) continue;
    const tag = raw.toLowerCase();
    if (tag.startsWith("pt")) return "pt-BR";
    if (tag.startsWith("en")) return "en-US";
  }
  return DEFAULT_LOCALE;
}

export type SetLanguageBridge = (locale: Locale) => Promise<void>;

/** Calls the Rust `set_language` command; a no-op outside Tauri (vite preview, tests). */
export const tauriSetLanguage: SetLanguageBridge = async (locale) => {
  if (!("__TAURI_INTERNALS__" in globalThis)) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke(SET_LANGUAGE_COMMAND, { language: locale });
};

let bridge: SetLanguageBridge = tauriSetLanguage;

export interface InitOptions {
  /** User preference (prefs override); wins over the OS locale. */
  override?: string | null | undefined;
  /** OS locale (navigator.language mirrors the Windows display language in WebView2). */
  osLocale?: string | null | undefined;
  /** Allows the `pseudo` locale (QA builds only). */
  allowPseudo?: boolean;
  setLanguage?: SetLanguageBridge;
}

function pick(options: InitOptions): AnyLocale {
  if (options.allowPseudo && options.override === PSEUDO_LOCALE) return PSEUDO_LOCALE;
  const osLocale =
    options.osLocale ?? (typeof navigator === "undefined" ? null : navigator.language);
  return resolveLocale(options.override, osLocale);
}

/** Initializes the shared i18next instance (idempotent) and syncs Rust (best-effort, off the render path). */
export async function initI18n(options: InitOptions = {}): Promise<I18n> {
  if (options.setLanguage) bridge = options.setLanguage;
  const lng = pick(options);
  if (i18next.isInitialized) {
    await i18next.changeLanguage(lng);
  } else {
    await i18next
      .use(lazyBackend)
      .use(initReactI18next)
      .init({
        lng,
        fallbackLng: DEFAULT_LOCALE,
        supportedLngs: [...SUPPORTED_LOCALES, PSEUDO_LOCALE],
        ns: [...CORE_NAMESPACES],
        defaultNS: DEFAULT_NAMESPACE,
        // Same {var} syntax as the Rust side (src-tauri/src/i18n.rs).
        interpolation: { escapeValue: false, prefix: "{", suffix: "}" },
      });
  }
  notifyRust(lng);
  return i18next;
}

/** Switches language at runtime: React re-renders via react-i18next, Rust via `set_language`. */
export async function setLanguage(lng: AnyLocale): Promise<void> {
  await i18next.changeLanguage(lng);
  notifyRust(lng);
}

/**
 * Best-effort, fire-and-forget Rust sync: never rejects and never blocks first paint or the
 * UI switch. A failing bridge (IPC error, ACL change) is only logged.
 */
function notifyRust(lng: AnyLocale): void {
  // Rust only knows shipped locales; the pseudo-locale keeps the en-US strings there.
  const target = lng === PSEUDO_LOCALE ? "en-US" : lng;
  try {
    bridge(target).catch((error: unknown) => {
      console.warn("i18n: set_language sync failed", error);
    });
  } catch (error) {
    console.warn("i18n: set_language sync failed", error);
  }
}

export function currentLocale(): AnyLocale {
  return i18next.language as AnyLocale;
}

export { i18next as i18n };
