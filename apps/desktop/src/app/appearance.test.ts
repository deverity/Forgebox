import { afterEach, describe, expect, it, vi } from "vitest";
import {
  applyAppearance,
  bootAppearance,
  followAppearance,
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
    expect(readBootAppearance(fakeWindow("?theme=sepia&vibrancy=maybe"))).toBeNull();
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
