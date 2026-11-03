import { describe, expect, it, vi } from "vitest";
import type * as T from "../ipc/generated";
import {
  activateTab,
  createTerminalsStore,
  openTerminal,
  reattach,
  reopenTab,
  removeTab,
  rememberSize,
  updateTab,
} from "./terminals";

function dto(id: string, patch: Partial<T.TerminalDto> = {}): T.TerminalDto {
  return {
    id,
    title: "Terminal",
    shell: "pwsh",
    cwd: "C:/Forgebox",
    cols: 80,
    rows: 24,
    pid: 1,
    started_at: "2026-11-03T10:00:00Z",
    last_seq: 0,
    ...patch,
  };
}

describe("terminal tabs", () => {
  it("openTerminal adds an opening tab, activates it and tells global, site and command apart", () => {
    const store = createTerminalsStore();
    const a = openTerminal({}, "local", store);
    const b = openTerminal({ site: "01SITE", siteHost: "loja.test" }, "local", store);
    const c = openTerminal({ command: ["mysql"], title: "MySQL" }, "local", store);
    const { tabs, activeKey } = store.getState();
    expect(tabs.map((t) => [t.kind, t.title, t.status])).toEqual([
      ["global", "", "opening"],
      ["site", "loja.test", "opening"],
      ["command", "MySQL", "opening"],
    ]);
    expect(activeKey).toBe(c);
    expect(new Set([a, b, c]).size).toBe(3); // ULID per tab (the `writer`)
  });

  it("closing the active tab activates its neighbour", () => {
    const store = createTerminalsStore();
    const a = openTerminal({}, "local", store);
    const b = openTerminal({}, "local", store);
    const c = openTerminal({}, "local", store);
    activateTab(b, store);
    removeTab(b, store);
    expect(store.getState().activeKey).toBe(c);
    removeTab(c, store);
    expect(store.getState().activeKey).toBe(a);
    removeTab(a, store);
    expect(store.getState().activeKey).toBeNull();
  });

  it("reopenTab clears the exit and bumps the epoch", () => {
    const store = createTerminalsStore();
    const key = openTerminal({ site: "s" }, "local", store);
    updateTab(key, { terminalId: "01X", status: "attached", exit: { code: 1 } }, store);
    reopenTab(key, store);
    expect(store.getState().tabs[0]).toMatchObject({
      terminalId: null,
      exit: null,
      status: "opening",
      epoch: 1,
      options: { site: "s" },
    });
  });

  it("remembers the last measured size and ignores empty ones", () => {
    const store = createTerminalsStore();
    rememberSize(100, 40, store);
    rememberSize(0, 0, store);
    expect(store.getState().lastSize).toEqual({ cols: 100, rows: 40 });
  });
});

describe("reattach", () => {
  it("adds a tab per listed session with its last_seq, skipping the ones already shown", async () => {
    const store = createTerminalsStore();
    const shown = openTerminal({}, "local", store);
    updateTab(shown, { terminalId: "01KNOWN", status: "attached" }, store);
    const api = {
      context: "local",
      call: vi.fn(() =>
        Promise.resolve({
          terminals: [
            dto("01KNOWN"),
            dto("01AAA", { last_seq: 77, site_id: "01SITE" }),
            dto("01BBB", { last_seq: 900, exit_code: 2, exited_at: "2026-11-03T10:01:00Z" }),
          ],
        }),
      ),
    };
    const added = await reattach(api as never, store);
    expect(added).toHaveLength(2);
    const tabs = store.getState().tabs;
    expect(tabs).toHaveLength(3);
    expect(tabs[1]).toMatchObject({ terminalId: "01AAA", lastSeq: 77, kind: "site", exit: null });
    expect(tabs[2]).toMatchObject({ terminalId: "01BBB", lastSeq: 900, exit: { code: 2 } });
    // A second pass finds nothing new.
    expect(await reattach(api as never, store)).toEqual([]);
  });
});
