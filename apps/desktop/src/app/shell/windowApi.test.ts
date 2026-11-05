import { afterEach, describe, expect, it, vi } from "vitest";
import {
  inertWindowApi,
  resetReadySignalForTests,
  sendReady,
  signalReadyAfterFirstFrame,
  tauriWindowApi,
} from "./windowApi";

// The Tauri window behind the dots (M2-E1-T23): every control and the focus state go through
// `getCurrentWindow()` (capabilities: minimize, toggle-maximize, close; is-focused and the
// focus events are in core:default).
const tauriWindow = vi.hoisted(() => {
  let focusHandler: ((event: { payload: boolean }) => void) | undefined;
  const win = {
    minimize: vi.fn(() => Promise.resolve()),
    toggleMaximize: vi.fn(() => Promise.resolve()),
    close: vi.fn(() => Promise.resolve()),
    isMaximized: vi.fn(() => Promise.resolve(false)),
    isFocused: vi.fn(() => Promise.resolve(false)),
    onResized: vi.fn(() => Promise.resolve(() => undefined)),
    onFocusChanged: vi.fn((handler: (event: { payload: boolean }) => void) => {
      focusHandler = handler;
      return Promise.resolve(() => undefined);
    }),
  };
  return { win, emitFocus: (payload: boolean) => focusHandler?.({ payload }) };
});
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => tauriWindow.win }));

afterEach(() => {
  resetReadySignalForTests();
});

describe("ready after the first frame", () => {
  it("waits two animation frames, then sends ready once", async () => {
    const frames: FrameRequestCallback[] = [];
    const raf = (cb: FrameRequestCallback) => frames.push(cb);
    const send = vi.fn(() => Promise.resolve());

    expect(signalReadyAfterFirstFrame(send, raf)).toBe(true);
    expect(send).not.toHaveBeenCalled();
    frames.shift()?.(0);
    expect(send).not.toHaveBeenCalled();
    frames.shift()?.(16);
    expect(send).toHaveBeenCalledOnce();

    // StrictMode runs effects twice; a second call is ignored.
    expect(signalReadyAfterFirstFrame(send, raf)).toBe(false);
    expect(frames).toHaveLength(0);
    await Promise.resolve();
    expect(send).toHaveBeenCalledOnce();
  });

  it("logs a failed delivery instead of throwing", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const raf = (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    };
    signalReadyAfterFirstFrame(() => Promise.reject(new Error("ipc down")), raf);
    await vi.waitFor(() => {
      expect(warn).toHaveBeenCalled();
    });
    warn.mockRestore();
  });

  it("is a no-op outside Tauri", async () => {
    await expect(sendReady()).resolves.toBeUndefined();
    await expect(inertWindowApi.isMaximized()).resolves.toBe(false);
  });
});

describe("Tauri window api", () => {
  it("drives the native window and follows its focus events", async () => {
    await tauriWindowApi.minimize();
    await tauriWindowApi.toggleMaximize();
    await tauriWindowApi.close();
    expect(tauriWindow.win.minimize).toHaveBeenCalledOnce();
    expect(tauriWindow.win.toggleMaximize).toHaveBeenCalledOnce();
    expect(tauriWindow.win.close).toHaveBeenCalledOnce();

    await expect(tauriWindowApi.isFocused()).resolves.toBe(false);
    const seen: boolean[] = [];
    await tauriWindowApi.onFocusChange((focused) => seen.push(focused));
    tauriWindow.emitFocus(true);
    tauriWindow.emitFocus(false);
    expect(seen).toEqual([true, false]);
  });

  it("is focused and inert outside Tauri", async () => {
    await expect(inertWindowApi.isFocused()).resolves.toBe(true);
    const stop = await inertWindowApi.onFocusChange(() => undefined);
    expect(() => {
      stop();
    }).not.toThrow();
  });
});
