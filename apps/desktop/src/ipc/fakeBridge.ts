// The one fake Bridge of every M2 test (vitest) and QA build (Playwright `?scenario=`).
// It emulates what the real bridge + daemon do, so UI code tested against it behaves the
// same against forgeboxd (ADR-0034):
// - calls are validated like src-tauri/src/bridge (unknown context/method, streams and
//   stream unsubscribes refused, params must be objects, `request_id` must be a ULID);
// - then like the daemon (forge-ipc server): role, `local_only` → `forbidden` when
//   `hello.local` is false, the not-implemented table → `not_implemented` with
//   `{reason: "not_implemented", args: {method, task, what}}`, idempotency of mutating
//   methods by `request_id`;
// - jobs answer a running `JobDto` at once, then publish `job.started → job.progress* →
//   job.done | job.failed | job.cancelled` in `seq` order (`jobs.get/cancel/list` work);
// - streams: per-stream `seq`, `resume_from` replay from a bounded history (older gaps are
//   `lagged`), a bounded per-subscription queue that drops the oldest while the consumer is
//   paused (`lagged` where the loss happened), frames of ≤ 256 entries, `reset` when the
//   sequence restarts, replay after a reconnection without loss or duplicates;
// - the connection: `ContextStateDto` + `onConnection`, disconnect/reconnect, deliberate
//   stop (`daemon.shutdown`), `start` with the uninitialized-root rule.
// Scenarios script hello, status, latency, handlers and history (`SCENARIOS`, `?scenario=`).
import {
  LOCAL_CONTEXT,
  type Bridge,
  type CallRequest,
  type ConnectionStatus,
  type ContextId,
  type ContextStateDto,
  type StartOptions,
  type StreamEntry,
  type StreamFrame,
  type StreamHandle,
  type Unlisten,
} from "./bridge";
import type { JobMethod, JobResultOf, ParamsOf, ResultOf } from "./api";
import { isUlid, ulid } from "./api";
import { ForgeApiError, apiError, type ApiErrorDto } from "./errors";
import notImplementedTable from "./fixtures/not-implemented.json";
import type * as T from "./generated";
import { API_VERSION, METHODS, type MethodMeta, type MethodName } from "./methods";

/** Methods the daemon answers `not_implemented` today (mirror of
 *  `forge_daemon::api::not_implemented::NOT_IMPLEMENTED`, checked against a real daemon's
 *  `hello.not_implemented` by src-tauri/tests/bridge_daemon.rs). */
export const NOT_IMPLEMENTED: readonly { method: string; task: string; what: string }[] =
  notImplementedTable;

const MAX_FRAME_ENTRIES = 256;
const ROLE_RANK: Record<T.Role, number> = { viewer: 0, operator: 1, admin: 2 };

// --- scenario ----------------------------------------------------------------------------

/** What a scenario handler sees. */
export interface FakeCallContext {
  context: ContextId;
  method: string;
  requestId: string;
  fake: FakeBridge;
}

const JOB_SCRIPT = Symbol("fakeJob");

/** How a fake job runs. */
export interface FakeJobScript<R = unknown> {
  /** `job.progress` payloads, one per step. */
  steps?: readonly T.JobProgress[];
  /** Delay before each step and before the outcome (default: scenario `jobStepMs`). */
  stepMs?: number;
  /** Default `{ done: null }`. */
  outcome?: { done: R } | { failed: ApiErrorDto } | { cancelled: true };
  /** Runs until `jobs.cancel` instead of ending by itself. */
  hold?: boolean;
}

export interface FakeJob<R = unknown> extends FakeJobScript<R> {
  readonly [JOB_SCRIPT]: true;
}

/** A job answer for a handler of a `job` method. */
export function fakeJob<R>(script: FakeJobScript<R> = {}): FakeJob<R> {
  return { ...script, [JOB_SCRIPT]: true };
}

function isFakeJob(value: unknown): value is FakeJob {
  return typeof value === "object" && value !== null && JOB_SCRIPT in value;
}

/** Throws from a handler: the call rejects with this error. */
export function fakeError(
  code: string,
  message: string,
  reason?: string,
  args: Record<string, unknown> = {},
): never {
  throw new ForgeApiError(
    reason === undefined ? { code, message } : { code, message, details: { reason, args } },
  );
}

type Answer<M extends MethodName> = M extends JobMethod
  ? FakeJob<JobResultOf<M>> | JobResultOf<M>
  : ResultOf<M>;

export type FakeHandler<M extends MethodName = MethodName> = (
  params: ParamsOf<M>,
  ctx: FakeCallContext,
) => Answer<M> | Promise<Answer<M>>;

export type FakeHandlers = { [M in MethodName]?: FakeHandler<M> };

/** A recorded event of the history. */
export interface FakeEvent {
  kind: string;
  payload?: unknown;
}

export interface FakeScenario {
  name?: string;
  /** Contexts that exist (default `["local"]`). */
  contexts?: readonly ContextId[];
  /** Starting status (default `connected`). */
  status?: ConnectionStatus;
  /** `stopped_reason` when starting in `stopped`. */
  stoppedReason?: string;
  /** The root has `state/` (default true). */
  initialized?: boolean;
  hello?: Partial<T.HelloDto>;
  /** Methods answered `not_implemented` (default `NOT_IMPLEMENTED`). */
  notImplemented?: readonly string[];
  handlers?: FakeHandlers;
  /** Used for methods without a handler (default: reject `internal`/`fake_no_handler`). */
  fallback?: (method: string, params: unknown, ctx: FakeCallContext) => unknown;
  /** Delay of every call, ms. */
  latencyMs?: number;
  /** Delay of each job step, ms (default 0). */
  jobStepMs?: number;
  /** Events already published (replayable with `resume_from`). */
  events?: readonly FakeEvent[];
  /** Items kept per stream for replays (default 4096, like the daemon's ring). */
  historySize?: number;
  /** Items queued per subscription before the oldest are dropped (default 4096). */
  streamCapacity?: number;
}

// --- streams -----------------------------------------------------------------------------

interface LogItem {
  seq: number;
  /** Envelope fields besides `subscription_id`/`seq` (`event`, `line`, `sample`). */
  fields: Record<string, unknown>;
}

interface Topic {
  next: number;
  items: LogItem[];
}

type QueueEntry =
  | { kind: "item"; item: Record<string, unknown> }
  | { kind: "lagged"; missed: number }
  | { kind: "reset" };

/** Drop-oldest queue with lag markers in place (same rules as forge-ipc's). */
class DropOldestQueue {
  private entries: QueueEntry[] = [];
  private items = 0;
  private headMissed = 0;
  private readonly capacity: number;

  constructor(capacity: number) {
    this.capacity = Math.max(1, capacity);
  }

  push(entry: QueueEntry): void {
    if (entry.kind === "lagged") {
      const last = this.entries.at(-1);
      if (last?.kind === "lagged") last.missed += entry.missed;
      else if (last === undefined) this.headMissed += entry.missed;
      else this.entries.push(entry);
      return;
    }
    if (entry.kind === "item" && this.items >= this.capacity) {
      for (;;) {
        const dropped = this.entries.shift();
        if (dropped === undefined) break;
        if (dropped.kind === "lagged") this.headMissed += dropped.missed;
        else if (dropped.kind === "item") {
          this.items -= 1;
          this.headMissed += 1;
          break;
        }
      }
    }
    this.entries.push(entry);
    if (entry.kind === "item") this.items += 1;
  }

  /** Up to `max` entries in stream order. */
  take(max: number): StreamEntry[] {
    const out: StreamEntry[] = [];
    while (out.length < max) {
      if (this.headMissed > 0) {
        let missed = this.headMissed;
        this.headMissed = 0;
        const front = this.entries[0];
        if (front?.kind === "lagged") {
          missed += front.missed;
          this.entries.shift();
        }
        out.push({ kind: "lagged", missed });
        continue;
      }
      const entry = this.entries.shift();
      if (entry === undefined) break;
      if (entry.kind === "item") this.items -= 1;
      out.push(entry);
    }
    return out;
  }

  get empty(): boolean {
    return this.entries.length === 0 && this.headMissed === 0;
  }
}

interface FakeSubscription {
  id: string;
  context: ContextId;
  method: string;
  topic: string;
  matches: (fields: Record<string, unknown>) => boolean;
  /** Daemon-side id (changes with every (re)subscription). */
  serverId: string;
  /** Last seq queued; resumes after it. */
  cursor: number;
  attached: boolean;
  queue: DropOldestQueue;
  frame: number;
  onFrame: (frame: StreamFrame) => void;
  closed: boolean;
  /** Subscribed while down: waits for the connection. */
  pending: boolean;
  params: Record<string, unknown>;
}

function topicOf(method: string, params: Record<string, unknown>): string {
  if (method === "logs.subscribe") return `logs:${String(params.target)}`;
  return method.split(".")[0] ?? method;
}

function eventMatcher(
  params: Record<string, unknown>,
): (fields: Record<string, unknown>) => boolean {
  const filter = (params.filter ?? {}) as T.EventFilter;
  const kinds = filter.kinds ?? [];
  const wanted: [string, string | null | undefined][] = [
    ["job_id", filter.job_id],
    ["site_id", filter.site_id],
    ["instance_id", filter.instance_id],
  ];
  return (fields) => {
    const event = fields.event as T.EventDto;
    const kindOk =
      kinds.length === 0 ||
      kinds.some((pattern) =>
        pattern.endsWith("*")
          ? event.kind.startsWith(pattern.slice(0, -1))
          : pattern === event.kind,
      );
    if (!kindOk) return false;
    const payload = (event.payload ?? {}) as Record<string, unknown>;
    return wanted.every(
      ([field, want]) => want === undefined || want === null || payload[field] === want,
    );
  };
}

function matcherOf(
  method: string,
  params: Record<string, unknown>,
): (fields: Record<string, unknown>) => boolean {
  if (method === "events.subscribe") return eventMatcher(params);
  if (method === "logs.subscribe") {
    const grep = typeof params.grep === "string" ? params.grep : null;
    return (fields) => grep === null || (fields.line as T.LogLineDto).text.includes(grep);
  }
  if (method === "metrics.subscribe") {
    const site = typeof params.site === "string" ? params.site : null;
    return (fields) => {
      const sample = fields.sample as T.SiteStatsDto;
      return site === null || sample.site_id === site || sample.host === site;
    };
  }
  return () => true;
}

// --- the fake ----------------------------------------------------------------------------

/** A recorded call. */
export interface FakeCall {
  context: ContextId;
  method: string;
  params: unknown;
  requestId: string;
}

interface JobRecord {
  dto: T.JobDto;
  timers: ReturnType<typeof setTimeout>[];
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function nowIso(): string {
  return new Date().toISOString();
}

function reject(error: ApiErrorDto, origin: { method?: string; requestId?: string } = {}): never {
  throw new ForgeApiError(error, origin);
}

export class FakeBridge implements Bridge {
  readonly scenario: FakeScenario;
  /** Every call that reached the "daemon" (after the bridge's own validation). */
  readonly calls: FakeCall[] = [];
  private status: ConnectionStatus;
  private stoppedReason: string | null;
  private initialized: boolean;
  private hello: T.HelloDto;
  private readonly contexts: Set<ContextId>;
  private readonly notImplemented: Map<string, { task: string; what: string }>;
  private readonly topics = new Map<string, Topic>();
  private readonly subscriptions = new Map<string, FakeSubscription>();
  private readonly listeners = new Set<(state: ContextStateDto) => void>();
  private readonly jobs = new Map<string, JobRecord>();
  private readonly idempotency = new Map<string, { method: string; answer: Promise<unknown> }>();
  private lastError: ApiErrorDto | null = null;
  private version = 0;
  private generation = 1;
  private restarts = 0;
  private paused = false;
  private flushTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(scenario: FakeScenario = {}) {
    this.scenario = scenario;
    this.status = scenario.status ?? "connected";
    this.stoppedReason = scenario.status === "stopped" ? (scenario.stoppedReason ?? "user") : null;
    this.initialized = scenario.initialized ?? true;
    this.contexts = new Set(scenario.contexts ?? [LOCAL_CONTEXT]);
    const table = new Map(
      NOT_IMPLEMENTED.map((row) => [row.method, { task: row.task, what: row.what }]),
    );
    this.notImplemented = new Map(
      (scenario.notImplemented ?? NOT_IMPLEMENTED.map((row) => row.method)).map((method) => [
        method,
        table.get(method) ?? { task: "?", what: "this method" },
      ]),
    );
    for (const event of scenario.events ?? []) this.publish(event.kind, event.payload ?? {});
    this.hello = this.makeHello();
  }

  // --- Bridge ----------------------------------------------------------------------------

  state(context: ContextId): Promise<ContextStateDto> {
    this.checkContext(context);
    return Promise.resolve(this.snapshot(context));
  }

  async call(request: CallRequest): Promise<unknown> {
    const origin = { method: request.method, requestId: request.request_id };
    this.checkContext(request.context);
    const meta = this.validateCall(request.method, origin);
    const params = this.objectParams(request.params, request.method, origin);
    if (!isUlid(request.request_id)) {
      reject(
        apiError(
          "invalid_input",
          `${JSON.stringify(request.request_id)} is not a ULID request_id`,
          "invalid_request_id",
          {
            request_id: request.request_id,
          },
        ),
        origin,
      );
    }
    await this.latency();
    if (this.status !== "connected") reject(this.disconnected(request.method), origin);
    this.calls.push({
      context: request.context,
      method: request.method,
      params,
      requestId: request.request_id,
    });
    this.authorize(request.method, meta, origin);
    if (meta.mutating) {
      const seen = this.idempotency.get(request.request_id);
      if (seen !== undefined) {
        if (seen.method !== request.method) {
          reject(
            apiError(
              "invalid_input",
              `request_id ${request.request_id} was used for ${seen.method}`,
            ),
            origin,
          );
        }
        return seen.answer;
      }
      const answer = this.execute(request, meta, params);
      this.idempotency.set(request.request_id, { method: request.method, answer });
      return answer;
    }
    return this.execute(request, meta, params);
  }

  async subscribe(
    context: ContextId,
    method: string,
    params: unknown,
    onFrame: (frame: StreamFrame) => void,
  ): Promise<StreamHandle> {
    this.checkContext(context);
    const origin = { method };
    const meta = this.known(method, origin);
    if (meta.kind !== "stream") {
      reject(
        apiError("invalid_input", `${method} is not a stream method`, "not_a_stream", { method }),
        origin,
      );
    }
    const object = this.objectParams(params, method, origin);
    await this.latency();
    const connected = this.status === "connected";
    if (connected) this.authorize(method, meta, origin);
    const sub: FakeSubscription = {
      id: ulid(),
      context,
      method,
      topic: topicOf(method, object),
      matches: matcherOf(method, object),
      serverId: ulid(),
      cursor: 0,
      attached: false,
      queue: new DropOldestQueue(this.scenario.streamCapacity ?? 4096),
      frame: 0,
      onFrame,
      closed: false,
      pending: !connected,
      params: object,
    };
    this.subscriptions.set(sub.id, sub);
    if (connected)
      this.attach(sub, typeof object.resume_from === "number" ? object.resume_from : null);
    return {
      id: sub.id,
      method,
      resumePoint: connected ? sub.cursor : null,
      pending: !connected,
      unsubscribe: () => {
        sub.closed = true;
        this.subscriptions.delete(sub.id);
        return Promise.resolve();
      },
    };
  }

  async start(context: ContextId, options: StartOptions = {}): Promise<ContextStateDto> {
    this.checkContext(context);
    if (this.status === "connected") return this.snapshot(context);
    if (!this.initialized && options.initialize !== true) {
      reject(
        apiError(
          "not_found",
          "the root is not initialized (no state/ folder)",
          "root_uninitialized",
          {
            root: "C:/Forgebox",
          },
        ),
      );
    }
    this.stoppedReason = null;
    this.setStatus("starting");
    await delay(this.scenario.latencyMs ?? 0);
    this.initialized = true;
    this.reconnect();
    return this.snapshot(context);
  }

  onConnection(listener: (state: ContextStateDto) => void): Promise<Unlisten> {
    this.listeners.add(listener);
    return Promise.resolve(() => {
      this.listeners.delete(listener);
    });
  }

  // --- control (tests and scenarios) -----------------------------------------------------

  /** Publishes an event (`events.subscribe` items); returns its seq. */
  publish(kind: string, payload: unknown = {}): number {
    const seq = this.append("events", (seq) => ({
      event: { id: ulid(), seq, ts: nowIso(), kind, payload } satisfies T.EventDto,
    }));
    return seq;
  }

  /** A log line of `target` (`logs.subscribe` items). */
  emitLog(target: string, line: Omit<T.LogLineDto, "seq">): number {
    return this.append(`logs:${target}`, (seq) => ({ line: { ...line, seq } }));
  }

  /** A metrics sample (`metrics.subscribe` items). */
  emitMetrics(sample: T.SiteStatsDto): number {
    return this.append("metrics", () => ({ sample }));
  }

  /** Last seq of a stream (`events`, `logs:<target>`, `metrics`). */
  lastSeq(topic = "events"): number {
    return (this.topics.get(topic)?.next ?? 1) - 1;
  }

  /** Stops delivering frames (a stalled webview): queues fill and drop their oldest. */
  pauseStreams(): void {
    this.paused = true;
  }

  resumeStreams(): void {
    this.paused = false;
    this.scheduleFlush();
  }

  /** The connection drops (the client reconnects; streams resume from their cursor). */
  disconnect(error: ApiErrorDto = this.disconnected()): void {
    for (const sub of this.subscriptions.values()) sub.attached = false;
    this.setStatus("reconnecting", error);
  }

  /** The connection is back: streams resubscribe from their cursor (no loss, no dupes). */
  reconnect(): void {
    this.generation += 1;
    this.hello = this.makeHello();
    this.setStatus("connected");
    for (const sub of [...this.subscriptions.values()]) {
      if (sub.pending) {
        sub.pending = false;
        try {
          this.authorize(sub.method, this.known(sub.method, { method: sub.method }), {
            method: sub.method,
          });
        } catch (error) {
          // Refused by the daemon it finally reached: the stream ends with the reason.
          this.subscriptions.delete(sub.id);
          sub.closed = true;
          sub.frame += 1;
          const reason =
            error instanceof ForgeApiError ? error.toJSON() : apiError("internal", String(error));
          sub.onFrame({
            stream_id: sub.id,
            frame: sub.frame,
            entries: [{ kind: "end", error: reason }],
          });
          continue;
        }
        this.attach(
          sub,
          typeof sub.params.resume_from === "number" ? sub.params.resume_from : null,
        );
      } else if (!sub.attached) {
        this.attach(sub, sub.cursor);
      }
    }
  }

  /** The daemon stops on purpose: `daemon.shutdown {reason}`, then `stopped`. */
  stop(reason = "user"): void {
    this.publish("daemon.shutdown", { v: 1, reason });
    this.flush();
    for (const sub of this.subscriptions.values()) sub.attached = false;
    this.stoppedReason = reason;
    this.setStatus("stopped", this.disconnected());
  }

  /** The daemon came back with a fresh state: every sequence restarts (`reset`). */
  resetStreams(): void {
    this.topics.clear();
    for (const sub of this.subscriptions.values()) {
      sub.cursor = 0;
      if (sub.attached) {
        sub.queue.push({ kind: "reset" });
        sub.serverId = ulid();
      }
    }
    this.scheduleFlush();
  }

  /** Any status, for scenarios (`uninitialized`, `starting`...). */
  setStatus(status: ConnectionStatus, error: ApiErrorDto | null = null): void {
    this.status = status;
    if (status !== "stopped")
      this.stoppedReason = status === "connected" ? null : this.stoppedReason;
    this.lastError = status === "connected" ? null : error;
    this.emitState();
  }

  /** Changes the hello (role, local...) as a new connection would. */
  setHello(patch: Partial<T.HelloDto>): void {
    this.scenario.hello = { ...this.scenario.hello, ...patch };
    this.hello = this.makeHello();
    this.emitState();
  }

  /** Jobs started so far. */
  jobList(): T.JobDto[] {
    return [...this.jobs.values()].map((job) => structuredClone(job.dto));
  }

  /** Streams open right now. */
  openStreams(): number {
    return this.subscriptions.size;
  }

  /** Delivers queued frames now (tests that do not want to wait a macrotask). */
  flush(): void {
    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    if (this.paused) return;
    for (const sub of [...this.subscriptions.values()]) {
      while (!sub.closed && !sub.queue.empty) {
        const entries = sub.queue.take(MAX_FRAME_ENTRIES);
        sub.frame += 1;
        sub.onFrame({ stream_id: sub.id, frame: sub.frame, entries });
      }
    }
  }

  // --- internals -------------------------------------------------------------------------

  private makeHello(): T.HelloDto {
    return {
      session_id: ulid(),
      api_version: API_VERSION,
      daemon_version: "0.0.0-fake",
      platform: "windows",
      role: "admin",
      transport: "in_memory",
      local: true,
      last_event_seq: this.lastSeq(),
      server_time: nowIso(),
      not_implemented: [...this.notImplemented.keys()],
      ...this.scenario.hello,
    };
  }

  private snapshot(context: ContextId): ContextStateDto {
    const connected = this.status === "connected";
    const status: ConnectionStatus =
      !connected && !this.initialized && this.status !== "starting" ? "uninitialized" : this.status;
    return {
      context,
      status,
      version: this.version,
      hello: connected ? structuredClone(this.hello) : null,
      generation: connected ? this.generation : null,
      attempt: status === "reconnecting" ? 1 : 0,
      next_retry_ms: 0,
      last_error: connected ? null : this.lastError,
      endpoint: "C:/Forgebox",
      autostart: {
        initialized: this.initialized,
        stopped_reason: connected ? null : this.stoppedReason,
        next_restart_ms: null,
        restarts: this.restarts,
      },
    };
  }

  private emitState(): void {
    this.version += 1;
    for (const context of this.contexts) {
      const state = this.snapshot(context);
      for (const listener of this.listeners) listener(state);
    }
  }

  private checkContext(context: ContextId): void {
    if (!this.contexts.has(context)) {
      reject(
        apiError("not_found", `unknown context ${JSON.stringify(context)}`, "unknown_context", {
          context,
        }),
      );
    }
  }

  private known(method: string, origin: { method?: string; requestId?: string }): MethodMeta {
    const meta = (METHODS as Record<string, MethodMeta | undefined>)[method];
    if (meta === undefined) {
      reject(
        apiError(
          "not_found",
          `${JSON.stringify(method)} is not a method of the API`,
          "unknown_method",
          { method },
        ),
        origin,
      );
    }
    return meta;
  }

  private validateCall(
    method: string,
    origin: { method?: string; requestId?: string },
  ): MethodMeta {
    const meta = this.known(method, origin);
    if (meta.kind === "stream") {
      reject(
        apiError("invalid_input", `${method} is a stream: use ipc_subscribe`, "use_subscribe", {
          method,
        }),
        origin,
      );
    }
    const [group, action] = method.split(".");
    const streamGroup = Object.entries(METHODS).some(
      ([name, m]) => m.kind === "stream" && name.split(".")[0] === group,
    );
    if (streamGroup && action === "unsubscribe") {
      reject(
        apiError(
          "invalid_input",
          `${method} is managed by the bridge: use ipc_unsubscribe`,
          "use_unsubscribe",
          { method },
        ),
        origin,
      );
    }
    return meta;
  }

  private objectParams(
    params: unknown,
    method: string,
    origin: { method?: string; requestId?: string },
  ): Record<string, unknown> {
    if (params === undefined || params === null) return {};
    if (typeof params === "object" && !Array.isArray(params))
      return params as Record<string, unknown>;
    return reject(
      apiError("invalid_input", `the params of ${method} must be an object`, "params_not_object", {
        method,
      }),
      origin,
    );
  }

  /** The daemon's checks, in its order: role, `local_only`, not implemented. */
  private authorize(
    method: string,
    meta: MethodMeta,
    origin: { method?: string; requestId?: string },
  ): void {
    const role = this.hello.role;
    if (ROLE_RANK[role] < ROLE_RANK[meta.min_role]) {
      reject(
        apiError(
          "forbidden",
          `${method} requires role ${meta.min_role} (this connection is ${role})`,
        ),
        origin,
      );
    }
    if (meta.local_only && !this.hello.local) {
      reject(
        apiError(
          "forbidden",
          `${method} is local_only: it acts on the daemon's machine and is not available to remote clients`,
        ),
        origin,
      );
    }
    const missing = this.notImplemented.get(method);
    if (missing !== undefined) {
      reject(
        apiError(
          "not_implemented",
          `not implemented: ${method} is not implemented yet: ${missing.what} (roadmap ${missing.task})`,
          "not_implemented",
          { method, task: missing.task, what: missing.what },
        ),
        origin,
      );
    }
  }

  private disconnected(method?: string): ApiErrorDto {
    return apiError(
      "disconnected",
      "the daemon is not reachable",
      "daemon_unreachable",
      method === undefined ? {} : { method },
    );
  }

  private async latency(): Promise<void> {
    const ms = this.scenario.latencyMs ?? 0;
    if (ms > 0) await delay(ms);
  }

  private async execute(
    request: CallRequest,
    meta: MethodMeta,
    params: Record<string, unknown>,
  ): Promise<unknown> {
    const ctx: FakeCallContext = {
      context: request.context,
      method: request.method,
      requestId: request.request_id,
      fake: this,
    };
    const origin = { method: request.method, requestId: request.request_id };
    const handler = (
      this.scenario.handlers as Record<string, FakeHandler | undefined> | undefined
    )?.[request.method];
    let answer: unknown;
    if (handler !== undefined) {
      answer = await handler(params, ctx);
    } else {
      const builtin = this.builtin(request.method, params);
      if (builtin !== undefined) answer = builtin.value;
      else if (this.scenario.fallback !== undefined)
        answer = await this.scenario.fallback(request.method, params, ctx);
      else if (meta.kind === "job") answer = fakeJob();
      else {
        reject(
          apiError(
            "internal",
            `fakeBridge scenario ${this.scenario.name ?? "(unnamed)"} has no handler for ${request.method}`,
            "fake_no_handler",
            { method: request.method },
          ),
          origin,
        );
      }
    }
    if (meta.kind === "job") {
      return this.startJob(
        request,
        isFakeJob(answer) ? answer : fakeJob({ outcome: { done: answer } }),
      );
    }
    return structuredClone(answer);
  }

  /** What the daemon answers by itself (`system.*`, `jobs.*`, `events.list`). */
  private builtin(method: string, params: Record<string, unknown>): { value: unknown } | undefined {
    switch (method) {
      case "system.ping":
        return { value: { server_time: nowIso() } satisfies T.PongDto };
      case "system.hello":
        return { value: this.hello };
      case "jobs.get": {
        const job = this.jobs.get(String(params.job_id));
        if (job === undefined)
          reject(apiError("not_found", `job ${String(params.job_id)} not found`));
        return { value: job.dto };
      }
      case "jobs.list": {
        const jobs = this.jobList().filter(
          (job) => params.active !== true || job.state === "running",
        );
        return { value: { jobs } satisfies T.JobListDto };
      }
      case "jobs.cancel": {
        const job = this.jobs.get(String(params.job_id));
        if (job === undefined)
          reject(apiError("not_found", `job ${String(params.job_id)} not found`));
        const running = job.dto.state === "running";
        if (running) this.finishJob(job, { cancelled: true });
        return { value: { job_id: job.dto.id, was_running: running } satisfies T.JobCancelDto };
      }
      case "events.list": {
        const after = typeof params.after_seq === "number" ? params.after_seq : 0;
        const events = (this.topics.get("events")?.items ?? [])
          .filter((item) => item.seq > after)
          .map((item) => item.fields.event as T.EventDto);
        const last = this.lastSeq();
        return {
          value: {
            events,
            next_after: last,
            last_seq: last,
            has_more: false,
          } satisfies T.EventPageDto,
        };
      }
      default:
        return undefined;
    }
  }

  private startJob(request: CallRequest, script: FakeJobScript): T.JobDto {
    const id = ulid();
    const dto: T.JobDto = {
      id,
      name: request.method,
      request_id: request.request_id,
      state: "running",
      progress: {},
      cancel_requested: false,
      started_at: nowIso(),
      finished_at: null,
      result: null,
      error: null,
    };
    const job: JobRecord = { dto, timers: [] };
    this.jobs.set(id, job);
    // Like the JobRegistry: `job.started` is published before the answer leaves.
    this.publish("job.started", {
      v: 1,
      job_id: id,
      name: dto.name,
      request_id: request.request_id,
    });
    const stepMs = script.stepMs ?? this.scenario.jobStepMs ?? 0;
    let at = 0;
    for (const step of script.steps ?? []) {
      at += stepMs;
      job.timers.push(
        setTimeout(() => {
          if (job.dto.state !== "running") return;
          job.dto.progress = { ...step };
          this.publish("job.progress", { v: 1, job_id: id, ...step });
        }, at),
      );
    }
    if (script.hold !== true) {
      at += stepMs;
      job.timers.push(
        setTimeout(() => {
          this.finishJob(job, script.outcome ?? { done: null });
        }, at),
      );
    }
    return structuredClone(dto);
  }

  private finishJob(job: JobRecord, outcome: NonNullable<FakeJobScript["outcome"]>): void {
    if (job.dto.state !== "running") return;
    for (const timer of job.timers) clearTimeout(timer);
    const { dto } = job;
    dto.finished_at = nowIso();
    const base = { v: 1, job_id: dto.id, name: dto.name, duration_ms: 0 };
    if ("done" in outcome) {
      dto.state = "done";
      dto.result = outcome.done;
      this.publish("job.done", { ...base, result: outcome.done });
    } else if ("failed" in outcome) {
      dto.state = "failed";
      dto.error = {
        code: outcome.failed.code,
        message: outcome.failed.message,
        details: outcome.failed.details ?? null,
      };
      this.publish("job.failed", { ...base, error: dto.error });
    } else {
      dto.state = "cancelled";
      dto.cancel_requested = true;
      this.publish("job.cancelled", { ...base, forced: false });
    }
  }

  /** Appends to a stream's history and delivers to its attached subscriptions. */
  private append(topicName: string, make: (seq: number) => Record<string, unknown>): number {
    const topic = this.topics.get(topicName) ?? { next: 1, items: [] };
    this.topics.set(topicName, topic);
    const seq = topic.next;
    topic.next += 1;
    const item: LogItem = { seq, fields: make(seq) };
    topic.items.push(item);
    const keep = this.scenario.historySize ?? 4096;
    if (topic.items.length > keep) topic.items.splice(0, topic.items.length - keep);
    for (const sub of this.subscriptions.values()) {
      if (sub.attached && sub.topic === topicName) this.deliver(sub, item);
    }
    return seq;
  }

  private deliver(sub: FakeSubscription, item: LogItem): void {
    if (item.seq <= sub.cursor) return;
    sub.cursor = item.seq;
    if (!sub.matches(item.fields)) return;
    sub.queue.push({
      kind: "item",
      item: { subscription_id: sub.serverId, seq: item.seq, ...item.fields },
    });
    this.scheduleFlush();
  }

  /** (Re)subscribes: replays what follows `from` (gaps older than the history are `lagged`). */
  private attach(sub: FakeSubscription, from: number | null): void {
    sub.serverId = ulid();
    sub.attached = true;
    const topic = this.topics.get(sub.topic);
    const head = (topic?.next ?? 1) - 1;
    if (from === null) {
      sub.cursor = head;
      return;
    }
    if (from > head) {
      // A cursor ahead of the daemon (its state was reset): the client reports one gap
      // and goes on live from the head.
      sub.queue.push({ kind: "lagged", missed: 1 });
      sub.cursor = head;
      this.scheduleFlush();
      return;
    }
    sub.cursor = from;
    const items = topic?.items ?? [];
    const oldest = items[0]?.seq ?? head + 1;
    if (oldest - 1 > from) {
      sub.queue.push({ kind: "lagged", missed: oldest - 1 - from });
      sub.cursor = oldest - 1;
    }
    for (const item of items) this.deliver(sub, item);
    this.scheduleFlush();
  }

  private scheduleFlush(): void {
    if (this.flushTimer !== null || this.paused) return;
    this.flushTimer = setTimeout(() => {
      this.flushTimer = null;
      this.flush();
    }, 0);
  }
}

// --- scenarios ---------------------------------------------------------------------------

export type ScenarioFactory = () => FakeScenario;

/** Named scenarios (`?scenario=<name>` in QA builds); feature tasks register their data. */
export const SCENARIOS = new Map<string, ScenarioFactory>([
  ["empty", () => ({ name: "empty" })],
  ["daemon-down", () => ({ name: "daemon-down", status: "stopped", stoppedReason: "user" })],
  ["uninitialized", () => ({ name: "uninitialized", status: "idle", initialized: false })],
  [
    "remote",
    () => ({
      name: "remote",
      hello: { local: false, role: "operator", transport: "web_socket_tls" },
    }),
  ],
  [
    "not-implemented",
    () => ({
      name: "not-implemented",
      notImplemented: Object.keys(METHODS).filter(
        (m) => !m.startsWith("system.") && !m.startsWith("events.") && !m.startsWith("jobs."),
      ),
    }),
  ],
  ["slow", () => ({ name: "slow", latencyMs: 1500, jobStepMs: 500 })],
  [
    "errors",
    () => ({
      name: "errors",
      fallback: (method) =>
        fakeError("internal", `${method} failed (scenario errors)`, "fake_error", { method }),
    }),
  ],
]);

/** Adds or replaces a named scenario. */
export function registerScenario(name: string, factory: ScenarioFactory): void {
  SCENARIOS.set(name, factory);
}

/** The scenario named by `?scenario=` (default `empty`); unknown names fall back to `empty`. */
export function scenarioFromSearch(search: string): FakeScenario {
  const name = new URLSearchParams(search).get("scenario") ?? "empty";
  const factory = SCENARIOS.get(name) ?? SCENARIOS.get("empty");
  return factory?.() ?? {};
}

export function createFakeBridge(scenario: FakeScenario = {}): FakeBridge {
  return new FakeBridge(scenario);
}
