import { afterEach, describe, expect, it, vi } from "vitest";
import {
  inertWindowApi,
  resetReadySignalForTests,
  sendReady,
  signalReadyAfterFirstFrame,
} from "./windowApi";

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
