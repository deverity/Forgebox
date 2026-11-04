// Helpers of the E2 live suite (M2-E2-T20): the CDP connection and the same Tauri commands the
// state layer uses (src/ipc/tauriBridge.ts). scripts/gui-e2e.ps1 exports the FORGE_E2E_* variables.
import { chromium, type Browser, type Page } from "@playwright/test";
import { ulid } from "../../src/ipc/api";

export function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run scripts/gui-e2e.ps1`);
  return value;
}

interface Internals {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
}

export async function invoke<T>(
  page: Page,
  cmd: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  return page.evaluate(
    ([c, a]) =>
      (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__.invoke(c, a),
    [cmd, args] as const,
  ) as Promise<T>;
}

/** One API call on the local daemon, like the GUI does it. */
export const call = <T>(page: Page, method: string, params: Record<string, unknown> = {}) =>
  invoke<T>(page, "ipc_call", { context: "local", method, params, request_id: ulid() });

export interface ContextState {
  status: string;
  hello: { local: boolean } | null;
}
export const ipcState = (page: Page) => invoke<ContextState>(page, "ipc_state", { context: "local" });

export async function connect(): Promise<{ browser: Browser; page: Page }> {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${need("FORGE_E2E_CDP_PORT")}`);
  const pages = browser.contexts().flatMap((c) => c.pages());
  const page = pages.find((p) => p.url().includes("tauri.localhost"));
  if (!page) throw new Error(`no tauri.localhost page among ${pages.map((p) => p.url()).join()}`);
  return { browser, page };
}
