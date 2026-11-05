import { describe, expect, it } from "vitest";
import {
  DEFAULT_SHORTCUTS,
  ShortcutRegistry,
  chordFromEvent,
  chordKeys,
  memoryStorage,
  normalizeChord,
} from "./registry";
import { describeShortcutError, globalChords, syncGlobalShortcuts } from "./useShortcuts";

function key(init: Partial<Parameters<typeof chordFromEvent>[0]> & { key: string }) {
  return chordFromEvent({
    code: "",
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...init,
  });
}

describe("defaults (docs/07 section 8)", () => {
  it("has every documented binding and no duplicate chord", () => {
    const chords = new Map(DEFAULT_SHORTCUTS.map((d) => [d.id, d.chord]));
    expect(chords.get("palette.open")).toBe("Ctrl+K");
    for (let n = 1; n <= 9; n += 1) {
      expect([...chords.values()]).toContain(`Ctrl+${String(n)}`);
    }
    expect(chords.get("services.startAll")).toBe("Ctrl+Shift+S");
    expect(chords.get("services.stopAll")).toBe("Ctrl+Shift+X");
    expect(chords.get("sites.new")).toBe("Ctrl+N");
    expect(chords.get("logs.open")).toBe("Ctrl+L");
    expect(chords.get("terminal.open")).toBe("Ctrl+`");
    expect(chords.get("settings.open")).toBe("Ctrl+,");
    expect(chords.get("terminal.global")).toBe("Ctrl+Alt+T");
    expect(new Set(chords.values()).size).toBe(chords.size);
  });
});

describe("chords", () => {
  it("normalizes and splits into key caps", () => {
    expect(normalizeChord("shift+ctrl+s")).toBe("Ctrl+Shift+S");
    expect(normalizeChord("control+,")).toBe("Ctrl+,");
    expect(normalizeChord("Ctrl+Nope")).toBeNull();
    expect(chordKeys("Ctrl+Shift+S")).toEqual(["Ctrl", "Shift", "S"]);
  });

  it("reads a keydown, using the physical key for backquote and digits", () => {
    expect(key({ key: "k", ctrlKey: true })).toBe("Ctrl+K");
    expect(key({ key: "Dead", code: "Backquote", ctrlKey: true })).toBe("Ctrl+`");
    expect(key({ key: "!", code: "Digit1", ctrlKey: true, shiftKey: true })).toBe("Ctrl+Shift+1");
    expect(key({ key: "a" })).toBeNull();
    expect(key({ key: "Control", ctrlKey: true })).toBeNull();
    expect(key({ key: "k", ctrlKey: true, metaKey: true })).toBeNull();
  });

  it("treats AltGr (Ctrl+Alt on pt-BR keyboards) as text, not a shortcut", () => {
    const altGr = (name: string) => name === "AltGraph";
    expect(
      key({ key: "/", code: "KeyQ", ctrlKey: true, altKey: true, getModifierState: altGr }),
    ).toBeNull();
    // The left Alt still makes Ctrl+Alt+T.
    expect(
      key({ key: "t", code: "KeyT", ctrlKey: true, altKey: true, getModifierState: () => false }),
    ).toBe("Ctrl+Alt+T");
  });
});

describe("overrides", () => {
  it("persists, replaces the old binding and survives a reload", async () => {
    const storage = memoryStorage();
    const registry = new ShortcutRegistry(storage);
    expect(registry.idOf("Ctrl+1")).toBe("nav.dashboard");
    const result = await registry.set("nav.dashboard", "ctrl+shift+1");
    expect(result).toEqual({ ok: true, chord: "Ctrl+Shift+1" });
    expect(registry.idOf("Ctrl+1")).toBeUndefined();
    expect(registry.idOf("Ctrl+Shift+1")).toBe("nav.dashboard");
    expect(storage.saved).toEqual({ "nav.dashboard": "Ctrl+Shift+1" });

    const reloaded = new ShortcutRegistry(storage);
    await reloaded.load();
    expect(reloaded.chordOf("nav.dashboard")).toBe("Ctrl+Shift+1");
    expect(reloaded.idOf("Ctrl+1")).toBeUndefined();
    expect(reloaded.bindings().find((b) => b.id === "nav.dashboard")?.overridden).toBe(true);
  });

  it("rejects a chord that another command uses, with a localized message", async () => {
    const storage = memoryStorage();
    const registry = new ShortcutRegistry(storage);
    const result = await registry.set("nav.dashboard", "Ctrl+2");
    expect(result).toEqual({
      ok: false,
      error: { kind: "conflict", chord: "Ctrl+2", with: "nav.services" },
    });
    expect(registry.chordOf("nav.dashboard")).toBe("Ctrl+1");
    expect(storage.saved).toEqual({});
    if (!result.ok) {
      const t = (k: string, v?: Record<string, string | number>) =>
        k === "shortcuts.conflict" ? `${String(v?.chord)} used by ${String(v?.name)}` : k;
      expect(describeShortcutError(result.error, t)).toBe(
        "Ctrl+2 used by shortcuts.names.nav.services",
      );
    }
  });

  it("refuses bare keys, bad chords and unknown ids, and resets to the default", async () => {
    const registry = new ShortcutRegistry(memoryStorage());
    expect((await registry.set("sites.new", "N")).ok).toBe(false);
    expect((await registry.set("sites.new", "Ctrl+Nope")).ok).toBe(false);
    expect((await registry.set("nope", "Ctrl+Q")).ok).toBe(false);
    expect((await registry.set("sites.new", "F6")).ok).toBe(true);
    expect((await registry.reset("sites.new")).ok).toBe(true);
    expect(registry.chordOf("sites.new")).toBe("Ctrl+N");
  });

  it("drops bad and clashing stored entries instead of failing", async () => {
    const registry = new ShortcutRegistry(
      memoryStorage({ "sites.new": "Ctrl+2", "logs.open": "garbage", nope: "Ctrl+Q" }),
    );
    await registry.load();
    // nav.services (earlier in registry order) keeps Ctrl+2; sites.new falls back to its default.
    expect(registry.chordOf("sites.new")).toBe("Ctrl+N");
    expect(registry.chordOf("logs.open")).toBe("Ctrl+L");
  });

  it("rolls back when the preferences cannot be saved", async () => {
    const registry = new ShortcutRegistry({
      load: () => Promise.resolve({}),
      save: () => Promise.reject(new Error("disk full")),
    });
    const result = await registry.set("sites.new", "Ctrl+Shift+N");
    expect(result).toEqual({ ok: false, error: { kind: "save_failed", message: "disk full" } });
    expect(registry.chordOf("sites.new")).toBe("Ctrl+N");
  });
});

describe("global shortcuts", () => {
  const bindings = new ShortcutRegistry(memoryStorage()).bindings();

  it("registers nothing while terminal.open is not implemented", async () => {
    expect(globalChords(bindings, false)).toEqual([]);
    const calls: unknown[] = [];
    const report = await syncGlobalShortcuts(
      (command, args) => {
        calls.push([command, args]);
        return Promise.resolve({ registered: [], failed: [] });
      },
      bindings,
      false,
    );
    expect(calls).toEqual([["shortcuts_sync", { enabled: false, chords: [] }]]);
    expect(report.failed).toEqual([]);
  });

  it("asks for Ctrl+` and Ctrl+Alt+T when it is implemented", () => {
    expect(globalChords(bindings, true)).toEqual(["Ctrl+`", "Ctrl+Alt+T"]);
  });

  it("turns a failing command into a report, never an exception", async () => {
    const report = await syncGlobalShortcuts(
      () => Promise.reject(new Error("boom")),
      bindings,
      true,
    );
    expect(report.registered).toEqual([]);
    expect(report.failed.map((f) => f.error)).toEqual(["boom", "boom"]);
  });
});
