// The native window as the titlebar sees it (ADR-0035), plus the first-frame signal.
// The Tauri window API is imported lazily so it stays out of the initial bundle; outside
// Tauri (vite preview, Playwright with the fake bridge, vitest) every action is a no-op.
import { useEffect } from "react";
import { isTauri } from "../appearance";

/** Name of the Rust command that shows the window after the first frame. */
export const READY_COMMAND = "ready";

export interface WindowApi {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  /** Asks to close: the shell hides to the tray or exits (close_to_tray). */
  close(): Promise<void>;
  isMaximized(): Promise<boolean>;
  /** Calls `onChange` after every resize with the maximized state; returns the unsubscribe. */
  onMaximizedChange(onChange: (maximized: boolean) => void): Promise<() => void>;
}

async function current() {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow();
}

export const tauriWindowApi: WindowApi = {
  async minimize() {
    await (await current()).minimize();
  },
  async toggleMaximize() {
    await (await current()).toggleMaximize();
  },
  async close() {
    await (await current()).close();
  },
  async isMaximized() {
    return (await current()).isMaximized();
  },
  async onMaximizedChange(onChange) {
    const win = await current();
    return win.onResized(() => {
      void win.isMaximized().then(onChange, () => undefined);
    });
  },
};

/** Inert window (browser, tests). */
export const inertWindowApi: WindowApi = {
  minimize: () => Promise.resolve(),
  toggleMaximize: () => Promise.resolve(),
  close: () => Promise.resolve(),
  isMaximized: () => Promise.resolve(false),
  onMaximizedChange: () => Promise.resolve(() => undefined),
};

export function defaultWindowApi(): WindowApi {
  return isTauri() ? tauriWindowApi : inertWindowApi;
}

type Raf = (callback: FrameRequestCallback) => number;

/** Sends `ready` to the shell (no-op outside Tauri). */
export async function sendReady(): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke(READY_COMMAND);
}

let readySignalled = false;

/**
 * After the first commit: wait two animation frames (the first runs before the paint of the
 * committed tree, the second after it) and tell the shell, which shows the window. Only the
 * first call of the page counts (StrictMode runs effects twice).
 */
export function signalReadyAfterFirstFrame(
  send: () => Promise<void> = sendReady,
  raf: Raf = (cb) => requestAnimationFrame(cb),
): boolean {
  if (readySignalled) return false;
  readySignalled = true;
  raf(() => {
    raf(() => {
      send().catch((error: unknown) => {
        console.warn("shell: ready not delivered", error);
      });
    });
  });
  return true;
}

/** Test hook: forget that `ready` was signalled. */
export function resetReadySignalForTests(): void {
  readySignalled = false;
}

/** Signals `ready` once the component that calls it has committed. */
export function useFirstFrameReady(): void {
  useEffect(() => {
    signalReadyAfterFirstFrame();
  }, []);
}
