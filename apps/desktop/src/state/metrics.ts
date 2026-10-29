// CPU/RAM/req/s for sparklines and the status bar (ADR-0034, state layer; docs/07 §4, §10).
// `services.status` is polled every 2 s, and only while at least one consumer is mounted
// and the window is visible and focused (0 calls otherwise); `daemon.status` joins the poll
// while a consumer wants the status-bar totals. Each instance and pool keeps a ring of the
// last 30 samples (60 s); pool req/s is the `requests_total` delta over the time between
// two polls. The persisted `service.metrics` events are not used: at one per instance per
// tick they would flood the daemon's resume ring.
import type { StoreApi } from "zustand/vanilla";
import type { ForgeApi } from "../ipc/api";
import type * as T from "../ipc/generated";
import type { PageActivity } from "./page";
import type {
  ConnectionSlice,
  InstanceSample,
  MetricsSlice,
  PoolSample,
  StatusTotals,
} from "./stores";

export const METRICS_INTERVAL_MS = 2000;
export const METRICS_SAMPLES = 30;

/** Pool req/s between two samples (`null` without a previous sample or after a counter reset). */
export function requestRate(
  previous: Pick<PoolSample, "t" | "requests"> | undefined,
  t: number,
  requests: number | null,
): number | null {
  if (previous === undefined || previous.requests === null || requests === null) return null;
  const seconds = (t - previous.t) / 1000;
  const delta = requests - previous.requests;
  if (seconds <= 0 || delta < 0) return null;
  return delta / seconds;
}

function push<S>(ring: readonly S[] | undefined, sample: S, size: number): readonly S[] {
  const next = ring === undefined ? [sample] : [...ring, sample];
  return next.length > size ? next.slice(next.length - size) : next;
}

/** The status-bar totals of one poll. */
export function statusTotals(
  status: T.ServiceStatusDto,
  daemon: T.DaemonStatusDto | null,
): StatusTotals {
  let stackCpu = 0;
  let stackRss = 0;
  let running = 0;
  for (const instance of status.instances) {
    stackCpu += instance.cpu_pct ?? 0;
    stackRss += instance.rss_bytes ?? 0;
    if (instance.state === "running") running += 1;
  }
  for (const pool of status.pools) {
    stackRss += pool.rss_bytes ?? 0;
    if (pool.state === "running") running += 1;
  }
  const daemonCpu = daemon?.cpu_pct ?? null;
  const daemonRss = daemon?.rss_bytes ?? null;
  return {
    daemonCpu,
    daemonRss,
    stackCpu,
    stackRss,
    cpu: stackCpu + (daemonCpu ?? 0),
    rss: stackRss + (daemonRss ?? 0),
    running,
  };
}

/** Appends one poll to the rings (rings of instances and pools that are gone are dropped). */
export function ingestMetrics(
  previous: MetricsSlice,
  status: T.ServiceStatusDto,
  daemon: T.DaemonStatusDto | null,
  t: number,
  size = METRICS_SAMPLES,
): MetricsSlice {
  const instances: Record<string, readonly InstanceSample[]> = {};
  for (const instance of status.instances) {
    instances[instance.id] = push(
      previous.instances[instance.id],
      { t, cpu: instance.cpu_pct, rss: instance.rss_bytes },
      size,
    );
  }
  const pools: Record<string, readonly PoolSample[]> = {};
  for (const pool of status.pools) {
    const ring = previous.pools[pool.version_id];
    pools[pool.version_id] = push(
      ring,
      {
        t,
        rps: requestRate(ring?.at(-1), t, pool.requests_total),
        requests: pool.requests_total,
        busy: pool.busy_workers,
        workers: pool.workers,
        rss: pool.rss_bytes,
      },
      size,
    );
  }
  return { instances, pools, totals: statusTotals(status, daemon), lastPollAt: t };
}

export interface MetricsPollerOptions {
  api: ForgeApi;
  store: StoreApi<MetricsSlice>;
  connection: StoreApi<ConnectionSlice>;
  page: PageActivity;
  intervalMs?: number;
  samples?: number;
  /** Methods the daemon does not implement (from `hello`). */
  notImplemented?: () => readonly string[];
  now?: () => number;
}

export interface MetricsConsumer {
  /** Also poll `daemon.status` for the status-bar totals. */
  daemon?: boolean;
}

/** Ref-counted poller of one context. */
export class MetricsPoller {
  private readonly options: MetricsPollerOptions;
  private readonly intervalMs: number;
  private readonly consumers = new Map<symbol, MetricsConsumer>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private inFlight = false;
  private lastPollAt: number | null = null;
  private readonly unsubscribe: (() => void)[] = [];
  private disposed = false;

  constructor(options: MetricsPollerOptions) {
    this.options = options;
    this.intervalMs = options.intervalMs ?? METRICS_INTERVAL_MS;
    this.unsubscribe.push(
      options.page.subscribe(() => {
        this.evaluate();
      }),
      options.connection.subscribe((state, previous) => {
        if (state.status !== previous.status) this.evaluate();
      }),
    );
  }

  /** Registers a consumer; the returned function releases it. */
  acquire(consumer: MetricsConsumer = {}): () => void {
    const key = Symbol("metrics-consumer");
    this.consumers.set(key, consumer);
    this.evaluate();
    return () => {
      this.consumers.delete(key);
      this.evaluate();
    };
  }

  isActive(): boolean {
    const { page, connection } = this.options;
    return (
      !this.disposed &&
      this.consumers.size > 0 &&
      page.visible() &&
      page.focused() &&
      connection.getState().status === "connected"
    );
  }

  dispose(): void {
    this.disposed = true;
    this.stop();
    for (const off of this.unsubscribe) off();
    this.consumers.clear();
  }

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  private evaluate(): void {
    if (!this.isActive()) {
      this.stop();
      return;
    }
    if (this.timer !== null) return;
    // Resume at once when the last sample is older than one interval, else on schedule.
    const since = this.lastPollAt === null ? Infinity : this.now() - this.lastPollAt;
    this.schedule(Math.max(0, this.intervalMs - since));
  }

  private stop(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private schedule(delay: number): void {
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.tick();
    }, delay);
  }

  private async tick(): Promise<void> {
    if (!this.isActive()) return;
    if (!this.inFlight) {
      this.inFlight = true;
      try {
        await this.poll();
      } catch (error) {
        console.warn("state: metrics poll failed", error);
      } finally {
        this.inFlight = false;
      }
    }
    if (this.isActive() && this.timer === null) this.schedule(this.intervalMs);
  }

  private async poll(): Promise<void> {
    const { api, store } = this.options;
    const skip = new Set(this.options.notImplemented?.() ?? []);
    const wantsDaemon =
      [...this.consumers.values()].some((c) => c.daemon === true) && !skip.has("daemon.status");
    this.lastPollAt = this.now();
    const [status, daemon] = await Promise.all([
      api.call("services.status", {}),
      wantsDaemon ? api.call("daemon.status", {}) : Promise.resolve(null),
    ]);
    if (this.disposed) return;
    const t = this.now();
    this.lastPollAt = t;
    store.setState(
      ingestMetrics(store.getState(), status, daemon, t, this.options.samples ?? METRICS_SAMPLES),
      true,
    );
  }
}
