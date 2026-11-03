// Live terminal tabs of the window: one emulator + session per tab, kept alive while the user
// visits other screens (the DOM element is moved in and out of the pane, never disposed), so
// the scrollback survives navigation. Disposed only when the tab is closed.
import { closeTerminal, openTerminal } from "../../actions/terminal";
import { runAction } from "../../actions/registry";
import type { ForgeApi } from "../../ipc/api";
import { toForgeApiError } from "../../ipc/errors";
import { copyText, pasteText } from "../../ipc/native";
import type { ReconnectQueue } from "../../state/reconnectQueue";
import {
  rememberSize,
  removeTab,
  terminalsStore,
  updateTab,
  type TerminalTab,
} from "../../state/terminals";
import type * as T from "../../ipc/generated";
import { TerminalSession, type SessionApi } from "./session";
import { createXterm, type XtermBundle } from "./xterm";

export interface RegistryDeps {
  api: ForgeApi;
  queue: ReconnectQueue;
}

export interface TerminalEntry {
  key: string;
  element: HTMLElement;
  bundle: XtermBundle;
  session: TerminalSession;
  /** `TerminalTab.epoch` this entry was started for. */
  startedEpoch: number;
  /** Set by the pane: opens the search bar (Ctrl+Shift+F). */
  openSearch: (() => void) | null;
}

const entries = new Map<string, TerminalEntry>();
const creating = new Map<string, Promise<TerminalEntry>>();

export function getEntry(key: string): TerminalEntry | undefined {
  return entries.get(key);
}

/** The session API over the typed client: actions for mutating calls, plain calls otherwise. */
export function sessionApi({ api, queue }: RegistryDeps): SessionApi {
  return {
    open: (params) => runAction(queue, openTerminal, params) as Promise<T.TerminalDto>,
    input: (params) => api.call("terminal.input", params),
    resize: (params) => api.call("terminal.resize", params),
    subscribe: (params, onEntries) =>
      api.subscribe("terminal.subscribe", params, (entriesOfFrame) => {
        onEntries(entriesOfFrame);
      }),
  };
}

/** Copy the selection, or paste when there is none (right click, like Windows Terminal). */
async function copyOrPaste(entry: TerminalEntry): Promise<void> {
  const { term } = entry.bundle;
  if (term.hasSelection()) {
    const text = term.getSelection();
    term.clearSelection();
    await copyText(text);
    return;
  }
  const text = await pasteText();
  if (text !== null && text !== "") term.paste(text);
}

function wireInteractions(entry: TerminalEntry): void {
  const { term } = entry.bundle;
  term.attachCustomKeyEventHandler((event) => {
    if (event.type !== "keydown" || !event.ctrlKey || !event.shiftKey || event.altKey) return true;
    if (event.code === "KeyC") {
      const text = term.getSelection();
      if (text !== "") void copyText(text);
      return false;
    }
    if (event.code === "KeyV") {
      void pasteText().then((text) => {
        if (text !== null && text !== "") term.paste(text);
      });
      return false;
    }
    if (event.code === "KeyF") {
      entry.openSearch?.();
      return false;
    }
    return true;
  });
  entry.element.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    void copyOrPaste(entry);
  });
}

async function build(
  tab: TerminalTab,
  host: HTMLElement,
  deps: RegistryDeps,
): Promise<TerminalEntry> {
  const element = document.createElement("div");
  element.className = "h-full w-full";
  element.dataset.terminalKey = tab.key;
  host.appendChild(element);
  const bundle = await createXterm(element);
  const measure = () => {
    try {
      bundle.fit.fit();
    } catch {
      /* hidden or zero-sized container: keep the last size */
    }
    const { lastSize } = terminalsStore.getState();
    return bundle.term.cols > 0
      ? { cols: bundle.term.cols, rows: bundle.term.rows }
      : { ...lastSize };
  };
  const session = new TerminalSession({
    writer: tab.key,
    term: bundle.term,
    api: sessionApi(deps),
    fit: measure,
    callbacks: {
      onOpened: (dto) => {
        const current = terminalsStore.getState().tabs.find((t) => t.key === tab.key);
        updateTab(tab.key, {
          terminalId: dto.id,
          status: "attached",
          lastSeq: dto.last_seq,
          error: null,
          ...(current?.title === "" ? { title: dto.title } : {}),
        });
      },
      onExit: (code) => {
        updateTab(tab.key, { exit: { code } });
      },
      onError: (error) => {
        updateTab(tab.key, { error: toForgeApiError(error).message });
      },
    },
  });
  const entry: TerminalEntry = {
    key: tab.key,
    element,
    bundle,
    session,
    startedEpoch: -1,
    openSearch: null,
  };
  bundle.term.onData((data) => {
    session.sendInput(data);
  });
  bundle.term.onResize(({ cols, rows }) => {
    rememberSize(cols, rows);
    session.resize(cols, rows);
  });
  wireInteractions(entry);
  entries.set(tab.key, entry);
  exposeForQa();
  return entry;
}

/**
 * The entry of `tab`, created on first use inside `host`; later calls move its element back
 * into `host` (the pane was remounted).
 */
export async function ensureEntry(
  tab: TerminalTab,
  host: HTMLElement,
  deps: RegistryDeps,
): Promise<TerminalEntry> {
  const existing = entries.get(tab.key);
  if (existing) {
    if (existing.element.parentElement !== host) host.appendChild(existing.element);
    return existing;
  }
  let pending = creating.get(tab.key);
  if (pending === undefined) {
    pending = build(tab, host, deps).finally(() => {
      creating.delete(tab.key);
    });
    creating.set(tab.key, pending);
  }
  const entry = await pending;
  if (entry.element.parentElement !== host) host.appendChild(entry.element);
  return entry;
}

/** Starts (or restarts, for "Reabrir") the session of `entry` according to its tab. */
export async function startEntry(entry: TerminalEntry, tab: TerminalTab): Promise<void> {
  if (entry.startedEpoch === tab.epoch) return;
  const reopening = entry.startedEpoch >= 0;
  entry.startedEpoch = tab.epoch;
  if (reopening) entry.session.resetForReopen();
  try {
    if (tab.terminalId === null) {
      const { options } = tab;
      await entry.session.open({
        ...(options.site === undefined ? {} : { site: options.site }),
        ...(options.dir === undefined ? {} : { dir: options.dir }),
        ...(options.shell === undefined ? {} : { shell: options.shell }),
        ...(options.command === undefined ? {} : { command: options.command }),
        ...(options.env === undefined ? {} : { env: options.env }),
        ...(options.versions === undefined ? {} : { versions: options.versions }),
        ...(tab.title === "" ? {} : { title: tab.title }),
      });
    } else if (tab.exit === null) {
      await entry.session.attach(tab.terminalId, tab.lastSeq);
    }
  } catch (error) {
    updateTab(tab.key, { status: "failed", error: toForgeApiError(error).message });
  }
}

/** Closes the tab: the process is killed on the daemon and the emulator disposed. */
export function closeEntry(key: string, deps: RegistryDeps): void {
  const tab = terminalsStore.getState().tabs.find((t) => t.key === key);
  const entry = entries.get(key);
  const id = entry?.session.terminalId ?? tab?.terminalId ?? null;
  if (entry) {
    entry.session.dispose();
    entry.bundle.dispose();
    entry.element.remove();
    entries.delete(key);
  }
  removeTab(key);
  if (id !== null) {
    runAction(deps.queue, closeTerminal, { id }).catch(() => undefined);
  }
}

/** Test helper: forgets every entry without touching the daemon. */
export function resetRegistryForTest(): void {
  for (const entry of entries.values()) {
    entry.session.dispose();
    entry.bundle.dispose();
  }
  entries.clear();
  creating.clear();
}

declare global {
  interface Window {
    __FORGEBOX_TERM__?: {
      text: (key?: string) => string;
      renderer: (key?: string) => string;
      /** The xterm `Terminal` of the tab (last one without `key`). */
      term: (key?: string) => unknown;
      /** Daemon id of the session of the tab. */
      id: (key?: string) => string | null;
    };
  }
}

/** QA builds only: lets Playwright read the emulator buffer (WebGL draws no DOM text). */
function exposeForQa(): void {
  if (import.meta.env.MODE !== "qa") return;
  const pick = (key?: string) =>
    key === undefined ? [...entries.values()].at(-1) : entries.get(key);
  window.__FORGEBOX_TERM__ = {
    text: (key) => {
      const buffer = pick(key)?.bundle.term.buffer.active;
      if (!buffer) return "";
      const lines: string[] = [];
      for (let i = 0; i < buffer.length; i += 1)
        lines.push(buffer.getLine(i)?.translateToString(true) ?? "");
      return lines.join("\n");
    },
    renderer: (key) => pick(key)?.bundle.renderer ?? "",
    term: (key) => pick(key)?.bundle.term,
    id: (key) => pick(key)?.session.terminalId ?? null,
  };
}
