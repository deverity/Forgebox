import { useCallback, useEffect, useRef, useState } from "react";
import type { ForgeApiError } from "../../ipc/errors";
import { toForgeApiError } from "../../ipc/errors";
import type { LogLineDto } from "../../ipc/generated";
import { useForgeApi } from "../../state";
import { LineMerger, TAIL_LINES, type LogRow } from "./model";

interface Inbox {
  target: string;
  lines: LogLineDto[];
  gaps: number[];
  reset: boolean;
}

const SUBSCRIBE = "logs.subscribe";

export interface LogStream {
  /** Rows as of the last frame that was not held back by the pause. */
  rows: readonly LogRow[];
  /** Lines that arrived since the viewer opened (grows even while paused). */
  arrived: number;
  /** All subscriptions answered (the first paint of an empty log is "empty", not "loading"). */
  loading: boolean;
  failure: ForgeApiError | null;
  paused: boolean;
  setPaused: (paused: boolean) => void;
  /** More older lines may exist (some target returned a full page last time). */
  hasEarlier: boolean;
  loadingEarlier: boolean;
  loadEarlier: () => Promise<void>;
}

/**
 * Follows `targets` with `logs.subscribe {tail: 200}` and merges them (M2-E3-T12). Entries are
 * queued as they arrive and applied once per animation frame, so a 1,000 lines/s stream costs
 * one render per frame. While paused the merge goes on but the list shown stays as it was.
 */
export function useLogStream(targets: readonly string[]): LogStream {
  const api = useForgeApi();
  const [merger] = useState(() => new LineMerger());
  const [rows, setRows] = useState<readonly LogRow[]>([]);
  const [arrived, setArrived] = useState(0);
  // Targets whose subscription answered (or failed): loading = some target still has to.
  const [ready, setReady] = useState<ReadonlySet<string>>(new Set());
  const [failures, setFailures] = useState<ReadonlyMap<string, ForgeApiError>>(new Map());
  const [readFailure, setReadFailure] = useState<ForgeApiError | null>(null);
  const [paused, setPausedState] = useState(false);
  const [hasEarlier, setHasEarlier] = useState(true);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const pausedRef = useRef(false);
  const inbox = useRef<Inbox[]>([]);
  const frame = useRef<number | null>(null);
  const total = useRef(0);
  const exhausted = useRef(new Set<string>());
  const active = useRef(new Map<string, { unsubscribe: () => Promise<void> } | "pending">());
  const targetsRef = useRef<readonly string[]>(targets);
  const key = targets.join("\n");

  const publish = useCallback(() => {
    setRows(merger.rows.slice());
  }, [merger]);

  const flush = useCallback(() => {
    frame.current = null;
    const batch = inbox.current;
    inbox.current = [];
    for (const entry of batch) {
      if (entry.reset) merger.remove(entry.target);
      total.current += merger.push(entry.target, entry.lines);
      for (const missed of entry.gaps) merger.gap(entry.target, missed);
    }
    setArrived(total.current);
    if (!pausedRef.current) publish();
  }, [merger, publish]);

  const schedule = useCallback(() => {
    frame.current ??= window.requestAnimationFrame(flush);
  }, [flush]);

  useEffect(() => {
    targetsRef.current = targets;
    const wanted = new Set(targets);
    let alive = true;
    let removed = false;
    for (const target of [...active.current.keys()]) {
      if (wanted.has(target)) continue;
      const handle = active.current.get(target);
      active.current.delete(target);
      merger.remove(target);
      removed = true;
      exhausted.current.delete(target);
      if (handle !== undefined && handle !== "pending")
        void handle.unsubscribe().catch(() => undefined);
    }
    for (const target of targets) {
      if (active.current.has(target)) continue;
      active.current.set(target, "pending");
      api
        .subscribe(SUBSCRIBE, { target, tail: TAIL_LINES }, (entries) => {
          const entry: Inbox = { target, lines: [], gaps: [], reset: false };
          for (const item of entries) {
            if (item.kind === "item") entry.lines.push(item.item.line);
            else if (item.kind === "lagged") entry.gaps.push(item.missed);
            else if (item.kind === "reset") entry.reset = true;
          }
          if (entry.lines.length + entry.gaps.length === 0 && !entry.reset) return;
          inbox.current.push(entry);
          schedule();
        })
        .then(
          (handle) => {
            if (active.current.get(target) === "pending") active.current.set(target, handle);
            else void handle.unsubscribe().catch(() => undefined);
          },
          (error: unknown) => {
            active.current.delete(target);
            if (alive) {
              setFailures((current) =>
                new Map(current).set(target, toForgeApiError(error, { method: SUBSCRIBE })),
              );
            }
          },
        )
        .finally(() => {
          if (alive) setReady((current) => new Set(current).add(target));
        });
    }
    // Removed sources leave the list: the next frame shows what is left.
    if (removed) schedule();
    return () => {
      alive = false;
    };
    // `key` stands for the list of targets (the array itself is a new object every render).
    // eslint-disable-next-line react-hooks/exhaustive-deps -- depending on `targets` would resubscribe on every render
  }, [api, key, merger, publish, schedule]);

  useEffect(() => {
    const live = active.current;
    return () => {
      if (frame.current !== null) window.cancelAnimationFrame(frame.current);
      for (const handle of live.values()) {
        if (handle !== "pending") void handle.unsubscribe().catch(() => undefined);
      }
      live.clear();
      inbox.current = [];
    };
  }, []);

  const setPaused = useCallback(
    (next: boolean) => {
      pausedRef.current = next;
      setPausedState(next);
      if (!next) publish();
    },
    [publish],
  );

  const loadEarlier = useCallback(async () => {
    setLoadingEarlier(true);
    try {
      let added = 0;
      for (const target of targetsRef.current) {
        const before = merger.oldestSeq(target);
        if (before === null || exhausted.current.has(target)) continue;
        const chunk = await api.call("logs.read", {
          target,
          limit: TAIL_LINES,
          before_seq: before,
        });
        if (chunk.lines.length < TAIL_LINES) exhausted.current.add(target);
        added += merger.prepend(target, chunk.lines);
      }
      if (added > 0) publish();
      setHasEarlier(
        targetsRef.current.some(
          (target) => merger.oldestSeq(target) !== null && !exhausted.current.has(target),
        ),
      );
    } catch (error) {
      setReadFailure(toForgeApiError(error, { method: "logs.read" }));
    } finally {
      setLoadingEarlier(false);
    }
  }, [api, merger, publish]);

  const loading = targets.some((target) => !ready.has(target));
  let failure: ForgeApiError | null = readFailure;
  for (const target of targets) failure ??= failures.get(target) ?? null;
  return {
    rows,
    arrived,
    loading,
    failure,
    paused,
    setPaused,
    hasEarlier,
    loadingEarlier,
    loadEarlier,
  };
}
