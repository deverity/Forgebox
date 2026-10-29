// Snapshots: the calls that (re)build the entity stores. Events keep the stores current;
// snapshots load them the first time, after a gap (`lagged`, `reset`, a reconnection to a
// daemon whose `last_event_seq` is below our cursor, a daemon restart), and per slice when a
// payload cannot build its entity (`site.created` → `sites.list`).
//
// Requests are debounced: everything asked within `resyncDebounceMs` becomes one round of
// calls, and a request that arrives while a round is in flight runs once more afterwards
// (the round in flight may predate the gap). Methods listed in `hello.not_implemented` are
// skipped; a failed call keeps the previous data and records the error per slice.
import type { CallMethod, ForgeApi, ParamsOf, ResultOf } from "../ipc/api";
import { toForgeApiError } from "../ipc/errors";
import type * as T from "../ipc/generated";
import { reconcile, type ContextStores, type SliceName } from "./stores";

interface Snapshot {
  readonly method: CallMethod;
  load(api: ForgeApi, stores: ContextStores): Promise<void>;
}

function snapshot<M extends CallMethod>(
  method: M,
  params: ParamsOf<M>,
  apply: (stores: ContextStores, result: ResultOf<M>) => void,
): Snapshot {
  return {
    method,
    async load(api, stores) {
      apply(stores, await api.call(method, params));
    },
  };
}

function mergeJobs(stores: ContextStores, list: readonly T.JobDto[]): void {
  const current = stores.jobs.getState().jobs;
  const next: Record<string, T.JobDto> = { ...current };
  let changed = false;
  for (const job of list) {
    const old = current[job.id];
    // A terminal state seen through events never goes back to running.
    if (old !== undefined && old.state !== "running" && job.state === "running") continue;
    const merged = reconcile(old === undefined ? {} : { [job.id]: old }, [job], (j) => j.id);
    const value = merged[job.id];
    if (value !== undefined && value !== old) {
      next[job.id] = value;
      changed = true;
    }
  }
  if (changed) stores.jobs.setState({ jobs: next });
}

/** What each slice loads from. */
export const SNAPSHOTS: Readonly<Record<SliceName, Snapshot>> = {
  services: snapshot("services.status", {}, (stores, result) => {
    const previous = stores.services.getState();
    stores.services.setState({
      instances: reconcile(previous.instances, result.instances, (i) => i.id),
      pools: reconcile(previous.pools, result.pools, (p) => p.version_id),
      loaded: true,
    });
  }),
  sites: snapshot("sites.list", {}, (stores, result) => {
    const previous = stores.sites.getState();
    stores.sites.setState({
      sites: reconcile(previous.sites, result.sites, (s) => s.id),
      loaded: true,
    });
  }),
  tools: snapshot("tools.list", { installed: true }, (stores, result) => {
    const previous = stores.tools.getState();
    stores.tools.setState({
      tools: reconcile(previous.tools, result.tools, (t) => t.id),
      loaded: true,
    });
  }),
  jobs: snapshot("jobs.list", {}, (stores, result) => {
    mergeJobs(stores, result.jobs);
  }),
  dns: snapshot("net.dns_status", {}, (stores, result) => {
    stores.system.setState({ dns: result });
  }),
  ca: snapshot("tls.ca_status", {}, (stores, result) => {
    stores.system.setState({ ca: result });
  }),
  daemon: snapshot("daemon.status", {}, (stores, result) => {
    stores.system.setState({ daemon: result });
  }),
};

export const SLICES = Object.keys(SNAPSHOTS) as SliceName[];

export interface SnapshotLoaderOptions {
  api: ForgeApi;
  stores: ContextStores;
  /** Methods the daemon does not implement (skipped). */
  notImplemented: () => readonly string[];
  /** Called once per completed full resync (queries are invalidated there). */
  onResync?: (reason: string) => void;
  debounceMs?: number;
}

export class SnapshotLoader {
  private readonly options: SnapshotLoaderOptions;
  private readonly debounceMs: number;
  private timer: ReturnType<typeof setTimeout> | null = null;
  /** Slices asked for and not yet loading. */
  private readonly wanted = new Set<SliceName>();
  private resyncReason: string | null = null;
  private running: Promise<void> | null = null;
  private disposed = false;

  constructor(options: SnapshotLoaderOptions) {
    this.options = options;
    this.debounceMs = options.debounceMs ?? 100;
  }

  /** Refetch every snapshot once (debounced). */
  resync(reason: string): void {
    this.resyncReason ??= reason;
    for (const slice of SLICES) this.wanted.add(slice);
    this.schedule();
  }

  /** Refetch one snapshot (debounced, merged with a pending resync). */
  refresh(slice: SliceName): void {
    this.wanted.add(slice);
    this.schedule();
  }

  /** Runs what is pending now; resolves when the store holds the answers. */
  async flush(): Promise<void> {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    if (this.running !== null) await this.running;
    if (this.wanted.size > 0) await this.run();
  }

  /** Resolves when nothing is pending or in flight. */
  async idle(): Promise<void> {
    while (this.running !== null || this.timer !== null || this.wanted.size > 0) {
      await this.flush();
    }
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.wanted.clear();
  }

  private schedule(): void {
    if (this.disposed || this.timer !== null || this.running !== null) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.run();
    }, this.debounceMs);
  }

  private run(): Promise<void> {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    const slices = [...this.wanted];
    const reason = this.resyncReason;
    this.wanted.clear();
    this.resyncReason = null;
    const round = this.load(slices, reason).finally(() => {
      this.running = null;
      // Asked for while this round was in flight: once more.
      if (this.wanted.size > 0) this.schedule();
    });
    this.running = round;
    return round;
  }

  private async load(slices: readonly SliceName[], reason: string | null): Promise<void> {
    const { api, stores } = this.options;
    const skip = new Set(this.options.notImplemented());
    const todo = slices.filter((slice) => !skip.has(SNAPSHOTS[slice].method));
    stores.sync.setState((s) => ({
      loading: { ...s.loading, ...Object.fromEntries(todo.map((slice) => [slice, true])) },
    }));
    const outcomes = await Promise.all(
      todo.map(async (slice) => {
        try {
          await SNAPSHOTS[slice].load(api, stores);
          return [slice, null] as const;
        } catch (error) {
          return [slice, toForgeApiError(error).toJSON()] as const;
        }
      }),
    );
    if (this.disposed) return;
    stores.sync.setState((s) => ({
      loading: { ...s.loading, ...Object.fromEntries(todo.map((slice) => [slice, false])) },
      errors: { ...s.errors, ...Object.fromEntries(outcomes) },
      ...(reason === null ? {} : { resyncs: s.resyncs + 1, lastResyncReason: reason }),
    }));
    if (reason !== null) this.options.onResync?.(reason);
  }
}
