import { afterEach, describe, expect, it, vi } from "vitest";
import {
  accentStates,
  applyAppearance,
  bootAppearance,
  followAppearance,
  luminance,
  onAccentColor,
  readBootAppearance,
  startedMinimized,
  type AppearanceSource,
  type ShellAppearance,
} from "./appearance";

const dark: ShellAppearance = {
  theme: "dark",
  vibrancy: "off",
  effect: "none",
  reason: "remote_session",
  contrast: "normal",
  accent: "#0078D7",
  remote_session: true,
  os_build: 19045,
};

function fakeWindow(search = "", boot?: Window["__FORGEBOX_SHELL__"]): Window {
  const doc = document.implementation.createHTMLDocument("t");
  return {
    location: { search } as Location,
    document: doc,
    __FORGEBOX_SHELL__: boot,
  } as unknown as Window;
}

afterEach(() => {
  const root = document.documentElement;
  for (const key of ["theme", "vibrancy", "effect", "contrast"]) {
    root.removeAttribute(`data-${key}`);
  }
  root.style.removeProperty("--system-accent");
  root.style.removeProperty("--accent");
  root.style.removeProperty("--fg-on-accent");
  root.style.removeProperty("--accent-hover");
  root.style.removeProperty("--accent-pressed");
});

describe("accent", () => {
  it.each(["#0078D4", "#FFB900", "#E81123", "#00CC6A", "#FFFFFF", "#000000", "#808080"])(
    "picks a text colour with at least 4.5:1 on %s",
    (accent) => {
      const text = onAccentColor(accent);
      const [a, b] = [luminance(accent), text === "white" ? 1 : 0].sort((p, q) => q - p) as [
        number,
        number,
      ];
      expect((a + 0.05) / (b + 0.05)).toBeGreaterThanOrEqual(4.5);
    },
  );

  it("drives --accent and --fg-on-accent, except in high contrast", () => {
    const root = document.documentElement;
    applyAppearance({ accent: "#FFB900" });
    expect(root.style.getPropertyValue("--accent")).toBe("#FFB900");
    expect(root.style.getPropertyValue("--fg-on-accent")).toBe("black");
    applyAppearance({ accent: "#FFB900", contrast: "high" });
    expect(root.style.getPropertyValue("--accent")).toBe("");
    expect(root.style.getPropertyValue("--fg-on-accent")).toBe("");
  });
});

describe("accent hover and pressed fills", () => {
  const ratio = (a: string, b: string) => {
    const [x, y] = [luminance(a), luminance(b)].sort((m, n) => n - m) as [number, number];
    return (x + 0.05) / (y + 0.05);
  };
  it.each(["#0078D4", "#FFB900", "#E81123", "#00CC6A", "#0071E3", "#0A84FF", "#808080"])(
    "keeps the text at >= 4.5:1 on %s in every state",
    (accent) => {
      const text = onAccentColor(accent) === "white" ? "#FFFFFF" : "#000000";
      const { hover, pressed } = accentStates(accent);
      expect(ratio(text, accent)).toBeGreaterThanOrEqual(4.5);
      expect(ratio(text, hover)).toBeGreaterThanOrEqual(ratio(text, accent));
      expect(ratio(text, pressed)).toBeGreaterThanOrEqual(ratio(text, hover));
    },
  );

  it("sets the tokens and clears them in high contrast", () => {
    const root = document.documentElement;
    applyAppearance({ accent: "#0078D4" });
    expect(root.style.getPropertyValue("--accent-hover")).toMatch(/^#[0-9a-f]{6}$/);
    expect(root.style.getPropertyValue("--accent-pressed")).toMatch(/^#[0-9a-f]{6}$/);
    applyAppearance({ accent: "#0078D4", contrast: "high" });
    expect(root.style.getPropertyValue("--accent-hover")).toBe("");
  });

  it("leaves the accent tokens to the Highlight colours under forced colours (F-10)", () => {
    const root = document.documentElement;
    const queries: string[] = [];
    vi.stubGlobal("matchMedia", (query: string) => {
      queries.push(query);
      return { matches: query === "(forced-colors: active)", media: query } as MediaQueryList;
    });
    try {
      applyAppearance({ accent: "#0078D4" });
      expect(queries).toContain("(forced-colors: active)");
      expect(root.style.getPropertyValue("--system-accent")).toBe("#0078D4");
      expect(root.style.getPropertyValue("--accent")).toBe("");
      expect(root.style.getPropertyValue("--fg-on-accent")).toBe("");
      expect(root.style.getPropertyValue("--accent-hover")).toBe("");
      expect(root.style.getPropertyValue("--accent-pressed")).toBe("");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("applies the accent when matchMedia is unavailable (jsdom, old webviews)", () => {
    vi.stubGlobal("matchMedia", undefined);
    try {
      applyAppearance({ accent: "#0078D4" });
      expect(document.documentElement.style.getPropertyValue("--accent")).toBe("#0078D4");
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("applyAppearance", () => {
  it("sets the data attributes and the system accent", () => {
    applyAppearance(dark);
    const root = document.documentElement;
    expect(root.dataset.theme).toBe("dark");
    expect(root.dataset.vibrancy).toBe("off");
    expect(root.dataset.effect).toBe("none");
    expect(root.dataset.contrast).toBeUndefined();
    expect(root.style.getPropertyValue("--system-accent")).toBe("#0078D7");
  });

  it("toggles high contrast and ignores a malformed accent", () => {
    const root = document.documentElement;
    applyAppearance({ ...dark, contrast: "high", accent: "red;}" });
    expect(root.dataset.contrast).toBe("high");
    expect(root.style.getPropertyValue("--system-accent")).toBe("");
    applyAppearance({ contrast: "normal" });
    expect(root.dataset.contrast).toBeUndefined();
    expect(root.dataset.theme).toBe("dark");
  });

  it("switches the material on and off", () => {
    applyAppearance({ ...dark, vibrancy: "on", effect: "mica" });
    expect(document.documentElement.dataset.vibrancy).toBe("on");
    applyAppearance({ vibrancy: "off", effect: "none" });
    expect(document.documentElement.dataset.vibrancy).toBe("off");
  });
});

describe("boot appearance", () => {
  it("prefers what the shell published", () => {
    const win = fakeWindow("?theme=light&vibrancy=on", { appearance: dark, minimized: true });
    expect(readBootAppearance(win)).toEqual(dark);
    expect(startedMinimized(win)).toBe(true);
    bootAppearance(win);
    expect(win.document.documentElement.dataset.theme).toBe("dark");
    expect(win.document.documentElement.dataset.vibrancy).toBe("off");
  });

  it("falls back to the URL (preview, Playwright) and validates it", () => {
    expect(readBootAppearance(fakeWindow("?theme=light&vibrancy=on"))).toEqual({
      theme: "light",
      vibrancy: "on",
    });
    // M2-E1-T23: the Windows 10 blur can be previewed too (the sidebar paints the glass tint).
    expect(readBootAppearance(fakeWindow("?vibrancy=on&effect=blur"))).toEqual({
      vibrancy: "on",
      effect: "blur",
    });
    expect(readBootAppearance(fakeWindow("?theme=sepia&vibrancy=maybe&effect=frosted"))).toBeNull();
    expect(readBootAppearance(fakeWindow(""))).toBeNull();
    expect(startedMinimized(fakeWindow(""))).toBe(false);
  });

  it("leaves the CSS defaults alone without information", () => {
    const win = fakeWindow("");
    bootAppearance(win);
    expect(win.document.documentElement.dataset.theme).toBeUndefined();
    expect(win.document.documentElement.dataset.vibrancy).toBeUndefined();
  });
});

describe("followAppearance", () => {
  it("listens first, then applies the current state, then every change", async () => {
    const calls: string[] = [];
    let emit: ((a: ShellAppearance) => void) | undefined;
    const unlisten = vi.fn();
    const source: AppearanceSource = {
      listen(onChange) {
        calls.push("listen");
        emit = onChange;
        return Promise.resolve(unlisten);
      },
      current() {
        calls.push("current");
        return Promise.resolve(dark);
      },
    };
    const seen: ShellAppearance[] = [];
    const stop = await followAppearance(source, (a) => seen.push(a));
    expect(calls).toEqual(["listen", "current"]);
    expect(seen).toEqual([dark]);
    emit?.({ ...dark, theme: "light" });
    expect(seen.at(-1)?.theme).toBe("light");
    stop();
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("applies to <html> by default and never rejects", async () => {
    const source: AppearanceSource = {
      listen: () => Promise.resolve(() => undefined),
      current: () => Promise.resolve({ ...dark, vibrancy: "on", effect: "acrylic" }),
    };
    await followAppearance(source);
    expect(document.documentElement.dataset.effect).toBe("acrylic");

    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const broken: AppearanceSource = {
      listen: () => Promise.reject(new Error("no ipc")),
      current: () => Promise.reject(new Error("no ipc")),
    };
    const stop = await followAppearance(broken);
    expect(typeof stop).toBe("function");
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it("is a no-op outside Tauri", async () => {
    const stop = await followAppearance(null);
    stop();
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });
});
