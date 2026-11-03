// Shortcut registry (docs/07 §8, M2-E1-T14): the default bindings, the user's overrides and the
// rules around them. Pure TypeScript, no React: `useShortcuts.ts` binds it to the keyboard and
// the settings editor (M2-E3-T16) reads and edits it through this API.
//
// A chord is written `Ctrl+Shift+S` (modifiers in the fixed order Ctrl, Alt, Shift, then the
// key). The key is one character (`K`, `1`, `,`, `` ` ``) or a named key (`F5`, `Enter`).
// Overrides are persisted in the GUI prefs under the key `shortcuts` ({id: chord}); a missing id
// has its default. A chord belongs to one command: setting one in use is refused (`conflict`).
import { FEATURES, type FeatureId } from "../features";

export type ShortcutId =
  | "palette.open"
  | `nav.${FeatureId}`
  | "services.startAll"
  | "services.stopAll"
  | "sites.new"
  | "logs.open"
  | "settings.open"
  | "terminal.open"
  | "terminal.global";

export interface ShortcutDef {
  id: ShortcutId;
  /** Default chord. */
  chord: string;
  /**
   * System-wide too (tauri-plugin-global-shortcut), only while the daemon implements
   * `terminal.open`. The in-app binding of the same chord still goes through the webview.
   */
  global?: boolean;
  /** Fires inside text inputs too (the palette must open from anywhere). */
  worksInInputs?: boolean;
}

/** `Ctrl+1..9` follow the sidebar order. */
const NAV_ORDER: readonly FeatureId[] = FEATURES.filter((f) => f.nav === "main").map((f) => f.id);

export const DEFAULT_SHORTCUTS: readonly ShortcutDef[] = [
  { id: "palette.open", chord: "Ctrl+K", worksInInputs: true },
  ...NAV_ORDER.slice(0, 9).map((id, index): ShortcutDef => ({
    id: `nav.${id}`,
    chord: `Ctrl+${String(index + 1)}`,
  })),
  { id: "services.startAll", chord: "Ctrl+Shift+S" },
  { id: "services.stopAll", chord: "Ctrl+Shift+X" },
  { id: "sites.new", chord: "Ctrl+N" },
  { id: "logs.open", chord: "Ctrl+L" },
  { id: "settings.open", chord: "Ctrl+," },
  { id: "terminal.open", chord: "Ctrl+`", global: true },
  { id: "terminal.global", chord: "Ctrl+Alt+T", global: true },
];

const DEFAULTS_BY_ID = new Map(DEFAULT_SHORTCUTS.map((d) => [d.id, d]));

export function shortcutDef(id: ShortcutId): ShortcutDef {
  const def = DEFAULTS_BY_ID.get(id);
  if (def === undefined) throw new Error(`unknown shortcut ${id}`);
  return def;
}

export function isShortcutId(value: string): value is ShortcutId {
  return DEFAULTS_BY_ID.has(value as ShortcutId);
}

// --- chords ---------------------------------------------------------------------------------

const MODIFIERS = ["Ctrl", "Alt", "Shift"] as const;

const NAMED_KEYS = new Map([
  ["enter", "Enter"],
  ["tab", "Tab"],
  ["space", "Space"],
  ["backspace", "Backspace"],
  ["delete", "Delete"],
  ["home", "Home"],
  ["end", "End"],
  ["pageup", "PageUp"],
  ["pagedown", "PageDown"],
  ["arrowup", "Up"],
  ["up", "Up"],
  ["arrowdown", "Down"],
  ["down", "Down"],
  ["arrowleft", "Left"],
  ["left", "Left"],
  ["arrowright", "Right"],
  ["right", "Right"],
]);

/** Canonical form of a key name (`k` -> `K`, `f5` -> `F5`, `arrowup` -> `Up`), or `null`. */
function normalizeKey(raw: string): string | null {
  if (raw.length === 1) return raw === " " ? "Space" : raw.toUpperCase();
  const lower = raw.toLowerCase();
  if (/^f([1-9]|1\d|2[0-4])$/.test(lower)) return lower.toUpperCase();
  return NAMED_KEYS.get(lower) ?? null;
}

export interface ParsedChord {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  key: string;
}

/** Parses `ctrl+shift+s` (any case, any modifier order); `null` when it is not a chord. */
export function parseChord(text: string): ParsedChord | null {
  // "+" itself is a key: "Ctrl++".
  const plusKey = text.endsWith("++");
  const parts = (plusKey ? text.slice(0, -2) : text).split("+").map((p) => p.trim());
  const keyText = plusKey ? "+" : parts.pop();
  if (keyText === undefined || keyText === "") return null;
  const chord: ParsedChord = { ctrl: false, alt: false, shift: false, key: "" };
  for (const part of parts) {
    switch (part.toLowerCase()) {
      case "ctrl":
      case "control":
        chord.ctrl = true;
        break;
      case "alt":
        chord.alt = true;
        break;
      case "shift":
        chord.shift = true;
        break;
      default:
        return null;
    }
  }
  const key = normalizeKey(keyText);
  if (key === null) return null;
  chord.key = key;
  return chord;
}

export function formatParsed(chord: ParsedChord): string {
  const mods = MODIFIERS.filter((m) => chord[m.toLowerCase() as "ctrl" | "alt" | "shift"]);
  return [...mods, chord.key].join("+");
}

/** Canonical text of `text`, or `null` when it is not a chord. */
export function normalizeChord(text: string): string | null {
  const parsed = parseChord(text);
  return parsed === null ? null : formatParsed(parsed);
}

/** Key caps for `<Kbd>`: `Ctrl+Shift+S` -> `["Ctrl", "Shift", "S"]`. */
export function chordKeys(chord: string): string[] {
  const parsed = parseChord(chord);
  if (parsed === null) return [chord];
  return [
    ...MODIFIERS.filter((m) => parsed[m.toLowerCase() as "ctrl" | "alt" | "shift"]),
    parsed.key,
  ];
}

interface KeyEventLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey?: boolean;
  getModifierState?: (key: string) => boolean;
}

/** Key of the physical position for the keys whose character depends on dead keys/layout. */
const CODE_KEYS: Record<string, string> = { Backquote: "`", Comma: ",", Period: ".", Slash: "/" };

/**
 * The chord a keydown stands for, or `null` when it is not a shortcut candidate (a lone
 * modifier, no Ctrl/Alt, a Windows-key combination) or when it is **AltGr**.
 *
 * AltGr: Windows reports AltGr as Ctrl+Alt (`getModifierState("AltGraph")` is true on pt-BR
 * keyboards), and `Ctrl+Alt+<key>` is how pt-BR users type characters such as `/`, `?` or `€`.
 * Such an event is text, never a shortcut. `Ctrl+Alt+T` with the left Alt still matches.
 */
export function chordFromEvent(event: KeyEventLike): string | null {
  if (event.metaKey === true) return null;
  if (event.getModifierState?.("AltGraph") === true) return null;
  if (!event.ctrlKey && !event.altKey) return null;
  // Digits come from the physical key: Shift+1 reports "!" on a US layout, "1" is the shortcut.
  const digit = /^Digit(\d)$/.exec(event.code)?.[1];
  const named = CODE_KEYS[event.code] ?? digit;
  const raw = named ?? (event.key === "Dead" || event.key === "Unidentified" ? "" : event.key);
  if (raw === "") return null;
  if (["Control", "Alt", "Shift", "Meta", "AltGraph"].includes(raw)) return null;
  const key = normalizeKey(raw);
  if (key === null) return null;
  return formatParsed({ ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey, key });
}

// --- registry -------------------------------------------------------------------------------

export type Overrides = Readonly<Record<string, string>>;

export interface ShortcutStorage {
  load(): Promise<Overrides>;
  save(overrides: Overrides): Promise<void>;
}

export type ShortcutError =
  | { kind: "invalid"; chord: string }
  | { kind: "needs_modifier"; chord: string }
  | { kind: "conflict"; chord: string; with: ShortcutId }
  | { kind: "unknown_id"; id: string }
  | { kind: "save_failed"; message: string };

export type SetResult = { ok: true; chord: string } | { ok: false; error: ShortcutError };

/** One row of the settings editor. */
export interface ShortcutBinding {
  id: ShortcutId;
  chord: string;
  defaultChord: string;
  overridden: boolean;
  global: boolean;
}

/** Memory storage (tests, QA builds without Tauri prefs). */
export function memoryStorage(initial: Overrides = {}): ShortcutStorage & { saved: Overrides } {
  const box = { saved: initial };
  return {
    get saved() {
      return box.saved;
    },
    load: () => Promise.resolve(box.saved),
    save: (overrides) => {
      box.saved = overrides;
      return Promise.resolve();
    },
  };
}

export class ShortcutRegistry {
  private overrides: Record<string, string> = {};
  private snapshot: readonly ShortcutBinding[] = [];
  private byChord = new Map<string, ShortcutId>();
  private readonly listeners = new Set<() => void>();

  constructor(private readonly storage: ShortcutStorage) {
    this.rebuild();
  }

  /** Reads the persisted overrides (invalid entries are dropped, not fatal). */
  async load(): Promise<void> {
    let stored: Overrides = {};
    try {
      stored = await this.storage.load();
    } catch {
      stored = {};
    }
    const next: Record<string, string> = {};
    for (const [id, chord] of Object.entries(stored)) {
      const normalized = typeof chord === "string" ? normalizeChord(chord) : null;
      if (isShortcutId(id) && normalized !== null) next[id] = normalized;
    }
    // Two commands on one chord (hand-edited file): the first in registry order keeps it.
    this.overrides = {};
    for (const def of DEFAULT_SHORTCUTS) {
      const chord = next[def.id];
      if (chord !== undefined && this.conflictOf(def.id, chord, next) === null) {
        this.overrides[def.id] = chord;
      }
    }
    this.rebuild();
    this.emit();
  }

  /** The effective bindings, in registry order (stable identity until something changes). */
  bindings(): readonly ShortcutBinding[] {
    return this.snapshot;
  }

  chordOf(id: ShortcutId): string {
    return this.overrides[id] ?? shortcutDef(id).chord;
  }

  /** The command a chord triggers right now, if any. */
  idOf(chord: string): ShortcutId | undefined {
    return this.byChord.get(chord);
  }

  /** Who owns `chord`, other than `id` (`null`: free). */
  conflictFor(id: ShortcutId, chord: string): ShortcutId | null {
    return this.conflictOf(id, chord, this.overrides);
  }

  /** Rebinds `id`. Refused (nothing changes) for an invalid chord or one that is in use. */
  async set(id: string, text: string): Promise<SetResult> {
    if (!isShortcutId(id)) return { ok: false, error: { kind: "unknown_id", id } };
    const chord = normalizeChord(text);
    if (chord === null) return { ok: false, error: { kind: "invalid", chord: text } };
    const parsed = parseChord(chord);
    // A bare letter would swallow typing; function keys are the only unmodified keys allowed.
    if (parsed !== null && !parsed.ctrl && !parsed.alt && !/^F\d+$/.test(parsed.key)) {
      return { ok: false, error: { kind: "needs_modifier", chord } };
    }
    const owner = this.conflictFor(id, chord);
    if (owner !== null) return { ok: false, error: { kind: "conflict", chord, with: owner } };
    const previous = this.overrides;
    // Back on the default chord: the override disappears instead of being stored.
    const kept = Object.entries(previous).filter(([key]) => key !== id);
    const next = Object.fromEntries(
      chord === shortcutDef(id).chord ? kept : [...kept, [id, chord]],
    );
    return this.commit(previous, next, chord);
  }

  /** Back to the default of `id`. Refused when another command took that chord. */
  async reset(id: string): Promise<SetResult> {
    if (!isShortcutId(id)) return { ok: false, error: { kind: "unknown_id", id } };
    return this.set(id, shortcutDef(id).chord);
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): readonly ShortcutBinding[] => this.snapshot;

  private async commit(
    previous: Record<string, string>,
    next: Record<string, string>,
    chord: string,
  ): Promise<SetResult> {
    this.overrides = next;
    this.rebuild();
    this.emit();
    try {
      await this.storage.save(next);
    } catch (error: unknown) {
      this.overrides = previous;
      this.rebuild();
      this.emit();
      const message = error instanceof Error ? error.message : String(error);
      return { ok: false, error: { kind: "save_failed", message } };
    }
    return { ok: true, chord };
  }

  private conflictOf(
    id: ShortcutId,
    chord: string,
    overrides: Readonly<Record<string, string>>,
  ): ShortcutId | null {
    for (const def of DEFAULT_SHORTCUTS) {
      if (def.id === id) continue;
      if ((overrides[def.id] ?? def.chord) === chord) return def.id;
    }
    return null;
  }

  private rebuild(): void {
    this.snapshot = DEFAULT_SHORTCUTS.map((def) => {
      const chord = this.overrides[def.id] ?? def.chord;
      return {
        id: def.id,
        chord,
        defaultChord: def.chord,
        overridden: chord !== def.chord,
        global: def.global === true,
      };
    });
    this.byChord = new Map(this.snapshot.map((b) => [b.chord, b.id]));
  }

  private emit(): void {
    this.listeners.forEach((l) => {
      l();
    });
  }
}

// --- default storage ------------------------------------------------------------------------

const PREFS_KEY = "shortcuts";
const LOCAL_KEY = "forgebox.shortcuts";

function inTauri(): boolean {
  return "__TAURI_INTERNALS__" in globalThis;
}

/**
 * The GUI prefs file through `gui_prefs_get/set` (ADR-0034 §5); outside Tauri (QA builds in a
 * plain browser) `localStorage`, so the behaviour is the same without a desktop.
 */
export const prefsStorage: ShortcutStorage = {
  async load() {
    if (inTauri()) {
      const { invoke } = await import("@tauri-apps/api/core");
      const prefs = await invoke<Record<string, unknown>>("gui_prefs_get");
      const value = prefs[PREFS_KEY];
      return typeof value === "object" && value !== null ? (value as Overrides) : {};
    }
    try {
      const text = globalThis.localStorage.getItem(LOCAL_KEY);
      return text === null ? {} : (JSON.parse(text) as Overrides);
    } catch {
      return {};
    }
  },
  async save(overrides) {
    if (inTauri()) {
      const { invoke } = await import("@tauri-apps/api/core");
      // `null` resets the key (no overrides left).
      await invoke("gui_prefs_set", {
        patch: { [PREFS_KEY]: Object.keys(overrides).length === 0 ? null : overrides },
      });
      return;
    }
    globalThis.localStorage.setItem(LOCAL_KEY, JSON.stringify(overrides));
  },
};

/** The registry of the app (settings editor, shell). */
export const shortcutRegistry = new ShortcutRegistry(prefsStorage);
