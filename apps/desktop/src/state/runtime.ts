// One context's data layer (ADR-0034, state layer): the typed API, the stores, the event
// stream batched per frame, snapshots and resync, query invalidation, the metrics poller,
// the job safety poll, the reconnect queue and the RTT probe. Nothing here knows a tool:
// it projects DTOs and events.
//
// Start order respects "paint first, connect later": `start()` runs from an effect after the
// first commit; it reads the context state (the bridge opens the context in the background),
// opens the event stream and loads the snapshots once the context is `connected`.
import { QueryClient } from "@tanstack/react-query";
import { ForgeApi } from "../ipc/api";
import type { Bridge, ContextId, ContextStateDto, Unlisten } from "../ipc/bridge";
import { toForgeApiError } from "../ipc/errors";
import type * as T from "../ipc/generated";
import { applyMutationEffect } from "./effects";
import { EventStream, type GapReason } from "./eventStream";
import { FrameBatcher, browserFrameClock, type FrameClock } from "./frame";
import { MetricsPoller } from "./metrics";
import { browserPageActivity, type PageActivity } from "./page";
import { Invalidator } from "./queries";
import { ReconnectQueue, RECONNECT_HOLD_MS } from "./reconnectQueue";
import { commitTx, openTx, reduceEvents } from "./reducers";
import { SnapshotLoader } from "./snapshots";
import { createContextStores, type ContextStores, type SliceName } from "./stores";

export interface RuntimeOptions {
  frame?: FrameClock;
  page?: PageActivity;
  queryClient?: QueryClient;
  /** Debounce of snapshot refetches (resync and per slice). */
  snapshotDebounceMs?: number;
  /** Debounce of query invalidations. */
  invalidateDebounceMs?: number;
  metricsIntervalMs?: number;
  metricsSamples?: number;
  /** Safety poll of `jobs.get` while a job is followed. */
  jobPollMs?: number;
  /** How long the reconnect queue holds a call. */
  holdMs?: number;
  /** `system.ping` period while connected and visible (0 disables). */
  rttIntervalMs?: number;
  maxBatch?: number;
  maxQueue?: number;
  now?: () => number;
}

export const RTT_INTERVAL_MS = 10_000;
export const JOB_POLL_MS = 1000;

export class ContextRuntime {
  readonly context: ContextId;
  readonly api: ForgeApi;
  readonly stores: ContextStores;
  readonly queryClient: QueryClient;
  readonly metrics: MetricsPoller;
  readonly queue: ReconnectQueue;
  readonly jobPollMs: number;
  private readonly bridge: Bridge;
  private readonly page: PageActivity;
  private readonly batcher: FrameBatcher<T.EventDto>;
  private readonly snapshots: SnapshotLoader;
  private readonly invalidator: Invalidator;
  private readonly stream: EventStream;
  private readonly options: RuntimeOptions;
  private unlisten: Unlisten | null = null;
  private started: Promise<void> | null = null;
  private loadedOnce = false;
  private rttTimer: ReturnType<typeof setTimeout> | null = null;
  private measuring = false;
  private readonly cleanups: (() => void)[] = [];
  private disposed = false;

  constructor(bridge: Bridge, context: ContextId, options: RuntimeOptions = {}) {
    this.bridge = bridge;
    this.context = context;
    this.options = options;
    this.jobPollMs = options.jobPollMs ?? JOB_POLL_MS;
    this.api = new ForgeApi(bridge, context, { jobPollMs: this.jobPollMs });
    this.stores = createContextStores(context);
    this.page = options.page ?? browserPageActivity();
    this.queryClient =
      options.queryClient ??
      new QueryClient({ defaultOptions: { queries: { staleTime: 30_000, retry: 1 } } });
    this.invalidator = new Invalidator(
      this.queryClient,
      context,
      options.invalidateDebounceMs ?? 200,
    );
    this.snapshots = new SnapshotLoader({
      api: this.api,
      stores: this.stores,
      notImplemented: () => this.stores.connection.getState().hello?.not_implemented ?? [],
      onResync: () => {
        this.invalidator.all();
      },
      ...(options.snapshotDebounceMs === undefined
        ? {}
        : { debounceMs: options.snapshotDebounceMs }),
    });
    this.batcher = new FrameBatcher<T.EventDto>({
      apply: (events) => {
        this.applyEvents(events);
      },
      frame: options.frame ?? browserFrameClock(),
      page: this.page,
      ...(options.maxBatch === undefined ? {} : { maxBatch: options.maxBatch }),
      ...(options.maxQueue === undefined ? {} : { maxQueue: options.maxQueue }),
      onOverflow: () => {
        this.resync("overflow");
      },
    });
    this.stream = new EventStream({
      api: this.api,
      onEvents: (events) => {
        this.batcher.push(events);
      },
      onGap: (reason: GapReason) => {
        this.resync(reason);
      },
      onCursor: (seq) => {
        this.stores.sync.setState({ cursor: seq });
      },
    });
    this.metrics = new MetricsPoller({
      api: this.api,
      store: this.stores.metrics,
      connection: this.stores.connection,
      page: this.page,
      notImplemented: () => this.stores.connection.getState().hello?.not_implemented ?? [],
      ...(options.metricsIntervalMs === undefined ? {} : { intervalMs: options.metricsIntervalMs }),
      ...(options.metricsSamples === undefined ? {} : { samples: options.metricsSamples }),
      ...(options.now === undefined ? {} : { now: options.now }),
    });
    this.queue = new ReconnectQueue(this.api, this.stores.connection, {
      holdMs: options.holdMs ?? RECONNECT_HOLD_MS,
      // A mutating call has no `job.done`: its `JOB_EFFECTS` row applies when it resolves.
      onCallDone: (method) => {
        this.applyMutationEffect(method);
      },
    });
  }

  /** Connects the stores to the context (idempotent). */
  start(): Promise<void> {
    this.started ??= this.boot();
    return this.started;
  }

  /** Refetch every snapshot once (debounced). */
  resync(reason: string): void {
    if (this.disposed) return;
    this.snapshots.resync(reason);
  }

  /** Refetch one snapshot (debounced). */
  refresh(slice: SliceName): void {
    if (this.disposed) return;
    this.snapshots.refresh(slice);
  }

  /**
   * Refetches the snapshots and invalidates the queries a finished mutation of `method`
   * makes stale (`JOB_EFFECTS`). Jobs get it from their `job.done|failed|cancelled` reducer;
   * the reconnect queue calls it for mutating calls.
   */
  applyMutationEffect(method: string): void {
    if (this.disposed) return;
    applyMutationEffect(
      {
        refresh: (slice) => {
          this.refresh(slice);
        },
        invalidate: (methods) => {
          this.invalidator.invalidate(methods);
        },
      },
      method,
    );
  }

  /** Applies queued events now (tests; the frame clock does it otherwise). */
  flushEvents(): void {
    this.batcher.flush();
  }

  /** Resolves when no snapshot is pending or loading. */
  settled(): Promise<void> {
    return this.snapshots.idle();
  }

  /** Reduces one batch: one commit per store that changed. */
  applyEvents(events: readonly T.EventDto[]): void {
    const tx = openTx(this.stores, {
      refresh: (slice) => {
        this.refresh(slice);
      },
      invalidate: (methods) => {
        this.invalidator.invalidate(methods);
      },
      resync: (reason) => {
        this.resync(reason);
      },
    });
    reduceEvents(tx, events);
    commitTx(tx);
    for (const event of events) this.invalidator.onEvent(event.kind);
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    this.unlisten?.();
    this.unlisten = null;
    for (const cleanup of this.cleanups.splice(0)) cleanup();
    if (this.rttTimer !== null) clearTimeout(this.rttTimer);
    this.rttTimer = null;
    this.batcher.dispose();
    this.snapshots.dispose();
    this.invalidator.dispose();
    this.metrics.dispose();
    this.queue.dispose();
    await this.stream.close();
  }

  private async boot(): Promise<void> {
    const unlisten = await this.bridge.onConnection((state) => {
      if (state.context === this.context) this.onState(state);
    });
    if (this.disposed) {
      unlisten();
      return;
    }
    this.unlisten = unlisten;
    this.cleanups.push(
      this.page.subscribe(() => {
        if (this.rttTimer === null && !this.measuring) this.scheduleRtt(0);
        else if (!this.page.visible()) this.scheduleRtt(0);
      }),
    );
    // Subscribed before the first snapshot, so nothing between the two is lost. Opened even
    // while the daemon is down: the bridge subscribes when it comes back.
    await this.stream.start();
    try {
      this.onState(await this.bridge.state(this.context));
    } catch (error) {
      console.warn(`state: ${this.context}: state failed`, toForgeApiError(error));
    }
  }

  private onState(state: ContextStateDto): void {
    if (this.disposed) return;
    const previous = this.stores.connection.getState();
    if (previous.state !== null && state.version < previous.state.version) return;
    const connected = state.status === "connected";
    const newConnection = connected && state.generation !== previous.generation;
    this.stores.connection.setState({
      status: state.status,
      state,
      hello: state.hello,
      generation: state.generation,
      ...(connected ? { shutdownReason: null } : {}),
      ...(connected ? {} : { rttMs: null }),
    });
    if (!newConnection) return;
    const cursor = this.stream.cursor;
    const daemonSeq = state.hello?.last_event_seq ?? 0;
    if (!this.loadedOnce) {
      this.loadedOnce = true;
      this.resync("initial");
    } else if (daemonSeq < cursor) {
      // The daemon's sequence is behind what we saw: its state is not the one we hold.
      this.stream.rewind(daemonSeq);
      this.resync("stale_seq");
    }
    this.scheduleRtt(0);
  }

  private scheduleRtt(delay: number): void {
    const period = this.options.rttIntervalMs ?? RTT_INTERVAL_MS;
    if (period <= 0 || this.disposed) return;
    if (this.rttTimer !== null) clearTimeout(this.rttTimer);
    this.rttTimer = null;
    if (this.stores.connection.getState().status !== "connected" || !this.page.visible()) return;
    this.rttTimer = setTimeout(() => {
      this.rttTimer = null;
      this.measuring = true;
      void this.measureRtt().finally(() => {
        this.measuring = false;
        this.scheduleRtt(period);
      });
    }, delay);
  }

  private async measureRtt(): Promise<void> {
    const now = this.options.now ?? (() => performance.now());
    const started = now();
    try {
      await this.api.call("system.ping", {}, { timeoutMs: 5000 });
      if (!this.disposed) this.stores.connection.setState({ rttMs: Math.max(0, now() - started) });
    } catch {
      // The connection state says why; RTT stays as it was.
    }
  }
}
