// Per-site metrics stores over `metrics.subscribe` (M2-E5-T14, ADR-0044, docs/14 section 4).
// One zustand store per (site, window); consumers share one subscription per store through a
// refcount, so two panels on the same site open one stream and the last unmount closes it.
// Stream entries are queued and applied once per animation frame: 100 items in a frame are
// one `setState` and one render. A site's store is separate from every other, so a sample of
// site B never notifies a consumer of site A.
//
// Snapshots: the daemon answers a (re)subscription with a fresh snapshot and does not replay
// the gap (ADR-0044; the charts refill it with `stats.series`). So after a reconnect or a
// `reset` entry the next samples REPLACE the points instead of being appended, and points are
// also deduplicated by `at` (a sample is never older than or equal to the last kept one).
import { createStore, type StoreApi } from "zustand/vanilla";
import type { ForgeApi } from "../../../ipc/api";
import { toForgeApiError, type ApiErrorDto } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";
import type { ConnectionSlice } from "../../../state";
import { FrameBatcher, browserFrameClock, type FrameClock } from "../../../state/frame";
import { browserPageActivity, type PageActivity } from "../../../state/page";
import { EMPTY_METRICS, MAX_POINTS, type MetricsPoint, type SiteMetricsState } from "./types";

const METHOD = "metrics.subscribe";

export interface MetricsHubOptions {
  api: ForgeApi;
  connection: StoreApi<ConnectionSlice>;
  /** Frame clock; defaults to rAF. Injectable for tests. */
  frame?: FrameClock;
  /** Page visibility; defaults to the browser page. A hidden page flushes on a timer. */
  page?: PageActivity;
  maxPoints?: number;
  /** Queue cap per store before the backlog is dropped and a snapshot awaited. */
  maxQueue?: number;
}

export type Inbound =
  | { kind: "sample"; sample: T.SiteStatsDto }
  | { kind: "lagged"; missed: number }
  | { kind: "reset" }
  | { kind: "end"; error: ApiErrorDto | null };

interface Entry {
  key: string;
  site: string;
  window: T.StatsWindow;
  store: StoreApi<SiteMetricsState>;
  refs: number;
  handle: { unsubscribe: () => Promise<void> } | "pending" | null;
  batcher: FrameBatcher<Inbound>;
  awaitingSnapshot: boolean;
}

export function toPoint(sample: T.SiteStatsDto): MetricsPoint | null {
  const t = Date.parse(sample.at);
  if (Number.isNaN(t)) return null;
  return {
    t,
    requests: sample.requests,
    rps: sample.rps,
    p50: sample.p50_ms,
    p90: sample.p90_ms,
    p99: sample.p99_ms,
    errorPct: sample.error_pct,
    cpu: sample.cpu_pct,
    mem: sample.mem_bytes,
  };
}

/** Applies a batch of stream entries to a state (pure; the hub commits the result once). */
export function applyEntries(
  state: SiteMetricsState,
  entries: readonly Inbound[],
  awaitingSnapshot: boolean,
  maxPoints = MAX_POINTS,
): { state: SiteMetricsState; awaitingSnapshot: boolean } {
  let next = state;
  let awaiting = awaitingSnapshot;
  let points: MetricsPoint[] | null = null;
  const own = (): MetricsPoint[] => (points ??= state.points.slice());
  for (const entry of entries) {
    if (entry.kind === "sample") {
      if (awaiting) {
        points = [];
        next = { ...next, missed: 0, snapshots: next.snapshots + 1 };
        awaiting = false;
      }
      const point = toPoint(entry.sample);
      const list = points ?? state.points;
      const last = list.at(-1);
      if (point !== null && (last === undefined || point.t > last.t)) {
        const target = own();
        target.push(point);
        if (target.length > maxPoints) target.splice(0, target.length - maxPoints);
      }
      next = { ...next, status: "live", latest: entry.sample, error: null };
    } else if (entry.kind === "lagged") {
      next = { ...next, missed: next.missed + entry.missed };
    } else if (entry.kind === "reset") {
      awaiting = true;
    } else {
      next = {
        ...next,
        status: "failed",
        error: entry.error === null ? null : toForgeApiError(entry.error, { method: METHOD }),
      };
    }
  }
  if (next === state && points === null) return { state, awaitingSnapshot: awaiting };
  return { state: { ...next, points: points ?? next.points }, awaitingSnapshot: awaiting };
}

export class MetricsHub {
  private readonly options: MetricsHubOptions;
  private readonly frame: FrameClock;
  private readonly page: PageActivity;
  private readonly entries = new Map<string, Entry>();
  private readonly offConnection: () => void;
  private everConnected: boolean;
  private disposed = false;

  constructor(options: MetricsHubOptions) {
    this.options = options;
    this.frame = options.frame ?? browserFrameClock();
    this.page = options.page ?? browserPageActivity();
    this.everConnected = options.connection.getState().status === "connected";
    this.offConnection = options.connection.subscribe((state, previous) => {
      if (state.status === previous.status || state.status !== "connected") return;
      // Back after a drop: the resubscription answers a snapshot that replaces the points.
      if (this.everConnected) {
        for (const entry of this.entries.values()) entry.awaitingSnapshot = true;
      }
      this.everConnected = true;
    });
  }

  /** Number of open subscriptions (live or being opened); for tests and diagnostics. */
  subscriptionCount(): number {
    let count = 0;
    for (const entry of this.entries.values()) if (entry.handle !== null) count += 1;
    return count;
  }

  /** State of the store of (site, window) if someone holds it. */
  peek(site: string, window: T.StatsWindow): SiteMetricsState | undefined {
    return this.entries.get(`${site}|${window}`)?.store.getState();
  }

  refCount(site: string, window: T.StatsWindow): number {
    return this.entries.get(`${site}|${window}`)?.refs ?? 0;
  }

  /** Takes a reference on the store of (site, window); the first one opens the stream. */
  acquire(
    site: string,
    window: T.StatsWindow,
  ): { store: StoreApi<SiteMetricsState>; release: () => void } {
    const key = `${site}|${window}`;
    let entry = this.entries.get(key);
    if (entry === undefined) {
      const created: Entry = {
        key,
        site,
        window,
        store: createStore<SiteMetricsState>(() => ({ ...EMPTY_METRICS, status: "pending" })),
        refs: 0,
        handle: null,
        batcher: null as unknown as FrameBatcher<Inbound>,
        awaitingSnapshot: false,
      };
      // docs/03 7.5: rAF while visible, a 250 ms timer while hidden (tray), flush on every
      // visibility change; past maxQueue the backlog is dropped and the next sample replaces
      // the points (as after a reconnect), so a hidden window never grows memory unbounded.
      created.batcher = new FrameBatcher<Inbound>({
        frame: this.frame,
        page: this.page,
        ...(this.options.maxQueue === undefined ? {} : { maxQueue: this.options.maxQueue }),
        apply: (batch) => {
          this.commit(created, batch);
        },
        onOverflow: () => {
          created.awaitingSnapshot = true;
        },
      });
      entry = created;
      this.entries.set(key, entry);
      this.open(entry);
    }
    entry.refs += 1;
    const held = entry;
    let released = false;
    return {
      store: held.store,
      release: () => {
        if (released) return;
        released = true;
        held.refs -= 1;
        if (held.refs <= 0) this.close(held);
      },
    };
  }

  dispose(): void {
    this.disposed = true;
    this.offConnection();
    for (const entry of [...this.entries.values()]) this.close(entry);
  }

  private open(entry: Entry): void {
    entry.handle = "pending";
    this.options.api
      .subscribe(METHOD, { site: entry.site, window: entry.window }, (items) => {
        if (this.entries.get(entry.key) !== entry) return;
        const batch: Inbound[] = [];
        for (const item of items) {
          if (item.kind === "item") batch.push({ kind: "sample", sample: item.item.sample });
          else if (item.kind === "lagged") batch.push({ kind: "lagged", missed: item.missed });
          else if (item.kind === "reset") batch.push({ kind: "reset" });
          else batch.push({ kind: "end", error: item.error });
        }
        entry.batcher.push(batch);
      })
      .then(
        (handle) => {
          if (entry.handle === "pending" && this.entries.get(entry.key) === entry)
            entry.handle = handle;
          else void handle.unsubscribe().catch(() => undefined);
        },
        (error: unknown) => {
          if (this.entries.get(entry.key) !== entry) return;
          entry.handle = null;
          entry.store.setState({
            ...entry.store.getState(),
            status: "failed",
            error: toForgeApiError(error, { method: METHOD }),
          });
        },
      );
  }

  private commit(entry: Entry, batch: readonly Inbound[]): void {
    if (this.disposed || batch.length === 0 || this.entries.get(entry.key) !== entry) return;
    const before = entry.store.getState();
    const result = applyEntries(
      before,
      batch,
      entry.awaitingSnapshot,
      this.options.maxPoints ?? MAX_POINTS,
    );
    entry.awaitingSnapshot = result.awaitingSnapshot;
    if (result.state !== before) entry.store.setState(result.state, true);
  }

  private close(entry: Entry): void {
    if (this.entries.get(entry.key) === entry) this.entries.delete(entry.key);
    entry.batcher.dispose();
    const handle = entry.handle;
    entry.handle = null;
    if (handle !== null && handle !== "pending") void handle.unsubscribe().catch(() => undefined);
  }
}

const hubs = new WeakMap<object, MetricsHub>();

/** The hub of a context runtime (created on first use). */
export function hubFor(owner: object, make: () => MetricsHubOptions): MetricsHub {
  let hub = hubs.get(owner);
  if (hub === undefined) {
    hub = new MetricsHub(make());
    hubs.set(owner, hub);
  }
  return hub;
}

export function disposeHub(owner: object): void {
  hubs.get(owner)?.dispose();
  hubs.delete(owner);
}
