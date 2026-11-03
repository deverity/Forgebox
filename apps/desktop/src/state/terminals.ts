// Terminal tabs (docs/07 §5.10, M2-E3-T15): which tabs exist, which is active and how each one
// ended. The sessions themselves (xterm, stream, input) live in `features/terminal`, which is a
// lazy chunk: this module stays free of xterm so the shell, the palette and the global shortcuts
// can open a terminal before the screen was ever loaded.
//
// `openTerminal` only adds a tab in the `opening` state; the screen mounts, measures the
// emulator with the fit addon and calls `terminal.open` with those cols/rows. A tab that
// `reattach` found on the daemon starts `attached` and resumes its stream.
import { createStore, type StoreApi } from "zustand/vanilla";
import { useStore } from "zustand";
import { ulid, type ForgeApi } from "../ipc/api";
import type { ContextId } from "../ipc/bridge";
import type * as T from "../ipc/generated";

export type TerminalTabKind = "global" | "site" | "command";

/** Options of `openTerminal` (a subset of `terminal.open`'s params plus the display title). */
export interface OpenTerminalOptions {
  /** Opens "in" a site (`terminal.open {site}`): per-site tab. */
  site?: string;
  /** Display name of the site (host) for the tab title. */
  siteHost?: string;
  /** Runs this argv instead of an interactive shell (`db.shell`, tools). */
  command?: string[];
  /** Tab title; derived from the site or the shell when absent. */
  title?: string;
  dir?: string;
  env?: Record<string, string>;
  versions?: Record<string, string>;
  shell?: string;
}

export type TerminalTabStatus = "opening" | "attached" | "failed";

export interface TerminalTab {
  /** Local identity and `writer` of `terminal.input` (a ULID per tab, reused across reconnects). */
  key: string;
  context: ContextId;
  kind: TerminalTabKind;
  title: string;
  options: OpenTerminalOptions;
  status: TerminalTabStatus;
  /** Daemon session id once opened/attached. */
  terminalId: string | null;
  /** Output offset to resume from (`TerminalDto.last_seq` of a reattached session). */
  lastSeq: number | null;
  /** Set once the process ended: the screen shows "Processo encerrado (código N) · Reabrir". */
  exit: { code: number | null } | null;
  /** Bumped by "Reabrir": the pane opens a fresh session. */
  epoch: number;
  siteId: string | null;
  error: string | null;
}

export interface TerminalsState {
  tabs: readonly TerminalTab[];
  activeKey: string | null;
  /** Dimensions the last emulator measured (used when a tab opens before any layout exists). */
  lastSize: { cols: number; rows: number };
}

function tabKind(options: OpenTerminalOptions): TerminalTabKind {
  if (options.command !== undefined && options.command.length > 0) return "command";
  return options.site === undefined ? "global" : "site";
}

function baseTitle(options: OpenTerminalOptions): string {
  if (options.title !== undefined && options.title !== "") return options.title;
  if (options.siteHost !== undefined) return options.siteHost;
  if (options.command !== undefined && options.command.length > 0) return options.command[0] ?? "";
  return "";
}

export const DEFAULT_SIZE = { cols: 120, rows: 30 } as const;

/** The store; exported for tests (the app uses the singleton below through hooks). */
export function createTerminalsStore(): StoreApi<TerminalsState> {
  return createStore<TerminalsState>(() => ({
    tabs: [],
    activeKey: null,
    lastSize: { ...DEFAULT_SIZE },
  }));
}

export const terminalsStore: StoreApi<TerminalsState> = createTerminalsStore();

export function useTerminals<R>(selector: (state: TerminalsState) => R): R {
  return useStore(terminalsStore, selector);
}

/** Adds a tab in `opening` state and activates it; returns its key. */
export function openTerminal(
  options: OpenTerminalOptions = {},
  context: ContextId = "local",
  store: StoreApi<TerminalsState> = terminalsStore,
): string {
  const key = ulid();
  const tab: TerminalTab = {
    key,
    context,
    kind: tabKind(options),
    title: baseTitle(options),
    options,
    status: "opening",
    terminalId: null,
    lastSeq: null,
    exit: null,
    epoch: 0,
    siteId: options.site ?? null,
    error: null,
  };
  store.setState((s) => ({ tabs: [...s.tabs, tab], activeKey: key }));
  return key;
}

export function updateTab(
  key: string,
  patch: Partial<TerminalTab>,
  store: StoreApi<TerminalsState> = terminalsStore,
): void {
  store.setState((s) => ({ tabs: s.tabs.map((t) => (t.key === key ? { ...t, ...patch } : t)) }));
}

export function activateTab(key: string, store: StoreApi<TerminalsState> = terminalsStore): void {
  store.setState({ activeKey: key });
}

/** Removes a tab; the neighbour becomes active when it was the active one. */
export function removeTab(key: string, store: StoreApi<TerminalsState> = terminalsStore): void {
  store.setState((s) => {
    const index = s.tabs.findIndex((t) => t.key === key);
    if (index < 0) return s;
    const tabs = s.tabs.filter((t) => t.key !== key);
    const activeKey =
      s.activeKey === key ? (tabs[Math.min(index, tabs.length - 1)]?.key ?? null) : s.activeKey;
    return { tabs, activeKey };
  });
}

/** "Reabrir": the pane opens a new session with the same options. */
export function reopenTab(key: string, store: StoreApi<TerminalsState> = terminalsStore): void {
  store.setState((s) => ({
    tabs: s.tabs.map((t) =>
      t.key === key
        ? {
            ...t,
            status: "opening",
            terminalId: null,
            lastSeq: null,
            exit: null,
            error: null,
            epoch: t.epoch + 1,
          }
        : t,
    ),
  }));
}

export function rememberSize(
  cols: number,
  rows: number,
  store: StoreApi<TerminalsState> = terminalsStore,
): void {
  if (cols > 0 && rows > 0) store.setState({ lastSize: { cols, rows } });
}

function sessionTitle(dto: T.TerminalDto): string {
  if (dto.title !== "") return dto.title;
  return dto.shell;
}

/**
 * Reattach (reload, reconnect): lists the daemon's sessions and adds a tab for each one the GUI
 * does not show yet, with `lastSeq` so its stream resumes right after what was produced.
 * Returns the keys of the tabs added.
 */
export async function reattach(
  api: Pick<ForgeApi, "call" | "context">,
  store: StoreApi<TerminalsState> = terminalsStore,
): Promise<string[]> {
  const list = await api.call("terminal.list", {});
  const known = new Set(
    store
      .getState()
      .tabs.filter((t) => t.context === api.context)
      .map((t) => t.terminalId)
      .filter((id): id is string => id !== null),
  );
  const added: TerminalTab[] = [];
  for (const dto of list.terminals) {
    if (known.has(dto.id)) continue;
    const exited = dto.exit_code !== undefined && dto.exit_code !== null;
    added.push({
      key: ulid(),
      context: api.context,
      kind: dto.site_id ? "site" : "global",
      title: sessionTitle(dto),
      options: dto.site_id ? { site: dto.site_id } : {},
      status: "attached",
      terminalId: dto.id,
      lastSeq: dto.last_seq,
      exit: exited || dto.exited_at ? { code: dto.exit_code ?? null } : null,
      epoch: 0,
      siteId: dto.site_id ?? null,
      error: null,
    });
  }
  if (added.length === 0) return [];
  store.setState((s) => ({
    tabs: [...s.tabs, ...added],
    activeKey: s.activeKey ?? added[0]?.key ?? null,
  }));
  return added.map((t) => t.key);
}

/** Test helper. */
export function resetTerminalsForTest(store: StoreApi<TerminalsState> = terminalsStore): void {
  store.setState({ tabs: [], activeKey: null, lastSize: { ...DEFAULT_SIZE } });
}
