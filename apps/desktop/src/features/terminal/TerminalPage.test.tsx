import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { terminalHandlers } from "../../../qa/fixtures/scenarios/terminal";
import { renderApp, type AppHarness } from "../../app/testApp";
import type * as T from "../../ipc/generated";
import { openTerminal, resetTerminalsForTest, terminalsStore } from "../../state/terminals";
import { resetRegistryForTest } from "./registry";

// xterm needs a real layout and canvas: the emulator is a recording fake. The session logic
// has its own tests (session.test.ts); this file covers the screen around it.
const noop = vi.hoisted(() => () => undefined);

vi.mock("./xterm", () => ({
  createXterm: vi.fn(() => {
    const term = {
      cols: 90,
      rows: 28,
      written: [] as string[],
      write(data: string, callback?: () => void) {
        term.written.push(data);
        callback?.();
      },
      reset() {
        term.written = [];
      },
      onData: () => ({ dispose: noop }),
      onResize: () => ({ dispose: noop }),
      attachCustomKeyEventHandler: noop,
      focus: noop,
      hasSelection: () => false,
      getSelection: () => "",
      clearSelection: noop,
      paste: noop,
      dispose: noop,
    };
    return Promise.resolve({
      term,
      fit: { fit: noop },
      search: { findNext: vi.fn(), findPrevious: vi.fn(), clearDecorations: vi.fn() },
      renderer: "dom",
      applyTheme: noop,
      dispose: noop,
    });
  }),
}));

let harness: AppHarness | null = null;

beforeEach(() => {
  resetTerminalsForTest();
});

afterEach(async () => {
  resetRegistryForTest();
  resetTerminalsForTest();
  await harness?.layer.dispose();
  harness = null;
});

function session(id: string, patch: Partial<T.TerminalDto> = {}): T.TerminalDto {
  return {
    id,
    title: `Terminal ${id}`,
    shell: "pwsh",
    cwd: "C:/Forgebox",
    site_id: null,
    cols: 100,
    rows: 30,
    pid: 10,
    started_at: "2026-11-03T10:00:00Z",
    last_seq: 0,
    ...patch,
  };
}

async function open(extra: Parameters<typeof renderApp>[0] = {}) {
  harness = await renderApp({ path: "/terminal", ...extra });
  await screen.findByTestId("page-terminal");
  return harness;
}

describe("Terminal screen", () => {
  it("reattaches two listed sessions as two tabs, each subscribed from its last seq", async () => {
    const list: T.TerminalListDto = {
      terminals: [session("01AAA", { last_seq: 120 }), session("01BBB", { last_seq: 4096 })],
    };
    harness = await renderApp({
      path: "/terminal",
      extra: { handlers: { "terminal.list": () => list } },
    });
    const subscribe = vi.spyOn(harness.fake, "subscribe");
    await screen.findByTestId("page-terminal");
    await waitFor(() => {
      expect(screen.getAllByRole("tab")).toHaveLength(2);
    });
    // The fake daemon has no output history, so it answers a cursor ahead of it with `lagged`
    // and the session replays the ring (`resume_from: null`): only the resumed ones are checked.
    const resumed = () =>
      subscribe.mock.calls
        .filter(([, method]) => method === "terminal.subscribe")
        .map(([, , p]) => p as T.TerminalSubscribeParams)
        .filter((p) => p.resume_from !== null && p.resume_from !== undefined);
    await waitFor(() => {
      expect(resumed()).toHaveLength(2);
    });
    expect(resumed()).toEqual(
      expect.arrayContaining([
        { id: "01AAA", resume_from: 120 },
        { id: "01BBB", resume_from: 4096 },
      ]),
    );
    expect(screen.getByRole("tab", { name: "Terminal 01AAA" })).toBeInTheDocument();
  });

  it("opens a new tab with the cols/rows of the fit and streams its output", async () => {
    const { fake } = await open({ extra: { handlers: terminalHandlers() } });
    await userEvent.setup().click(screen.getByRole("button", { name: "New terminal" }));
    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "terminal.open");
      expect(call?.params).toMatchObject({ cols: 90, rows: 28 });
    });
    await waitFor(() => {
      expect(screen.getByRole("tab", { name: "Terminal" })).toBeInTheDocument();
    });
  });

  it("opens a tab requested from outside (shortcut, palette, tray) before the screen exists", async () => {
    await open({ extra: { handlers: terminalHandlers() } });
    act(() => {
      openTerminal({ command: ["mysql", "-u", "root"], title: "MySQL" });
    });
    await screen.findByRole("tab", { name: "MySQL" });
    await waitFor(() => {
      const call = harness?.fake.calls.find((c) => c.method === "terminal.open");
      expect(call?.params).toMatchObject({ command: ["mysql", "-u", "root"], title: "MySQL" });
    });
    expect(terminalsStore.getState().tabs[0]?.kind).toBe("command");
  });

  it("shows 'Process ended (code N)' with Reopen, and Reopen opens a new session", async () => {
    const dead = session("01DEAD", { exit_code: 3, exited_at: "2026-11-03T10:05:00Z" });
    const handlers = { ...terminalHandlers(), "terminal.list": () => ({ terminals: [dead] }) };
    const { fake } = await open({ extra: { handlers } });
    const banner = await screen.findByTestId("terminal-exited");
    expect(banner).toHaveTextContent("Process ended (code 3)");
    await userEvent.setup().click(screen.getByRole("button", { name: "Reopen" }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "terminal.open")).toBe(true);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("terminal-exited")).toBeNull();
    });
  });

  it("closes a tab and kills the session on the daemon", async () => {
    const { fake } = await open({ extra: { handlers: terminalHandlers() } });
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "New terminal" }));
    await screen.findByRole("tab", { name: "Terminal" });
    await waitFor(() => {
      expect(terminalsStore.getState().tabs[0]?.terminalId).not.toBeNull();
    });
    await user.click(screen.getByRole("button", { name: /Close tab/ }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "terminal.close")).toBe(true);
    });
    expect(screen.queryByRole("tab")).toBeNull();
  });

  it("titles the screen with the context name on a remote daemon", async () => {
    await open({ scenario: "remote", extra: { handlers: terminalHandlers() } });
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Terminal · local");
  });
});
