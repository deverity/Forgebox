// Binds the shortcut registry to the keyboard and to the system (M2-E1-T14).
//
// * `useShortcuts(handlers, ready)`: one `keydown` listener on `window`. Inputs keep their native keys:
//   inside a text field, textarea, select or contenteditable only shortcuts flagged
//   `worksInInputs` (Ctrl+K) fire. AltGr (Ctrl+Alt on pt-BR keyboards) is text, see
//   `chordFromEvent`.
// * `useGlobalShortcuts()`: asks the Rust side (`shortcuts_sync`) for the system-wide chords of
//   the terminal, only while the daemon implements `terminal.open`. A registration failure is
//   reported once and is never fatal.
import { createContext, useContext, useEffect, useRef, useSyncExternalStore } from "react";
import {
  chordFromEvent,
  shortcutDef,
  shortcutRegistry,
  type ShortcutBinding,
  type ShortcutError,
  type ShortcutId,
  type ShortcutRegistry,
} from "./registry";

export const ShortcutRegistryContext = createContext<ShortcutRegistry>(shortcutRegistry);

export function useShortcutRegistry(): ShortcutRegistry {
  return useContext(ShortcutRegistryContext);
}

/** The effective bindings; re-renders when the user rebinds something. */
export function useShortcutBindings(): readonly ShortcutBinding[] {
  const registry = useShortcutRegistry();
  return useSyncExternalStore(registry.subscribe, registry.getSnapshot);
}

/** The effective chord of `id` as `Ctrl+K` text. */
export function useChord(id: ShortcutId): string {
  const bindings = useShortcutBindings();
  return bindings.find((b) => b.id === id)?.chord ?? shortcutDef(id).chord;
}

/** Whether keystrokes in `target` belong to the element (typing, caret movement, selection). */
export function isTextEntry(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable || target.getAttribute("contenteditable") === "true") return true;
  const tag = target.tagName;
  if (tag === "TEXTAREA" || tag === "SELECT") return true;
  if (tag === "INPUT") {
    const type = (target as HTMLInputElement).type;
    return !["button", "checkbox", "radio", "submit", "reset", "range", "color", "file"].includes(
      type,
    );
  }
  return target.getAttribute("role") === "textbox";
}

export type ShortcutHandlers = Partial<Record<ShortcutId, () => void>>;

/**
 * Installs the keyboard handler; `handlers` may change on every render. While `ready` is false
 * (the handlers' texts are still loading) a shortcut waits, in order, and runs once it is true:
 * it is never dropped.
 */
export function useShortcuts(handlers: ShortcutHandlers, ready = true): void {
  const registry = useShortcutRegistry();
  const ref = useRef(handlers);
  const readyRef = useRef(ready);
  const waiting = useRef<ShortcutId[]>([]);
  useEffect(() => {
    ref.current = handlers;
    readyRef.current = ready;
    if (ready) for (const id of waiting.current.splice(0)) ref.current[id]?.();
  });
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat) return;
      const chord = chordFromEvent(event);
      if (chord === null) return;
      const id = registry.idOf(chord);
      if (id === undefined) return;
      if (isTextEntry(event.target) && shortcutDef(id).worksInInputs !== true) return;
      const handler = ref.current[id];
      if (handler === undefined) return;
      event.preventDefault();
      if (readyRef.current) handler();
      else waiting.current.push(id);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [registry]);
}

// --- messages -------------------------------------------------------------------------------

type Translate = (key: string, values?: Record<string, string | number>) => string;

/** Localized text of a refused change (`palette` namespace). */
export function describeShortcutError(error: ShortcutError, t: Translate): string {
  switch (error.kind) {
    case "conflict":
      return t("shortcuts.conflict", {
        chord: error.chord,
        name: t(`shortcuts.names.${error.with}`),
      });
    case "invalid":
      return t("shortcuts.invalid", { chord: error.chord });
    case "needs_modifier":
      return t("shortcuts.needsModifier", { chord: error.chord });
    case "unknown_id":
      return t("shortcuts.unknown", { id: error.id });
    case "save_failed":
      return t("shortcuts.saveFailed", { message: error.message });
  }
}

// --- global shortcuts -----------------------------------------------------------------------

export const GLOBAL_SHORTCUT_EVENT = "forgebox://global-shortcut";
export const GLOBAL_SYNC_COMMAND = "shortcuts_sync";

export interface GlobalSyncReport {
  registered: string[];
  failed: { accelerator: string; error: string }[];
}

export type GlobalInvoke = (
  command: string,
  args: { enabled: boolean; chords: string[] },
) => Promise<GlobalSyncReport>;

/** What the webview asks the system to register for the given capability state. */
export function globalChords(
  bindings: readonly ShortcutBinding[],
  terminalImplemented: boolean,
): string[] {
  if (!terminalImplemented) return [];
  return bindings.filter((b) => b.global).map((b) => b.chord);
}

/**
 * Registers (or clears) the system-wide chords. Never throws: a failed command or chord comes
 * back in `failed`, and the caller reports it.
 */
export async function syncGlobalShortcuts(
  invoke: GlobalInvoke,
  bindings: readonly ShortcutBinding[],
  terminalImplemented: boolean,
): Promise<GlobalSyncReport> {
  const chords = globalChords(bindings, terminalImplemented);
  try {
    return await invoke(GLOBAL_SYNC_COMMAND, { enabled: terminalImplemented, chords });
  } catch (error: unknown) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      registered: [],
      failed: chords.map((accelerator) => ({ accelerator, error: message })),
    };
  }
}
