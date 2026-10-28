// Typed client over a Bridge (ADR-0034): `call<M>` and `job<M>` are typed from the
// generated method table, every user intent carries one ULID `request_id` that is reused
// when the same intent is retried (the daemon then answers instead of acting twice,
// ADR-0009 rule 4), and failures are `ForgeApiError`s.
//
// `job<M>` mirrors the CLI (ADR-0030): it subscribes to `job.*` *before* calling, so a
// job that ends before its answer arrives is still seen, follows the job's events and
// polls `jobs.get` every second as a safety net (and at once after a `lagged`/`reset`).
import {
  LOCAL_CONTEXT,
  type Bridge,
  type ContextId,
  type ContextStateDto,
  type StartOptions,
  type StreamEntry,
  type StreamFrame,
  type StreamHandle,
} from "./bridge";
import { ForgeApiError, toForgeApiError } from "./errors";
import type * as T from "./generated";
import type { ApiMethods, METHODS, MethodName } from "./methods";

type KindOf<M extends MethodName> = (typeof METHODS)[M]["kind"];

/** Methods answered at once (calls and jobs; jobs answer a `JobDto`). */
export type CallMethod = {
  [M in MethodName]: KindOf<M> extends "stream" ? never : M;
}[MethodName];
/** Long operations: `JobDto` now, `job_result` later (`job<M>`). */
export type JobMethod = { [M in MethodName]: KindOf<M> extends "job" ? M : never }[MethodName];
/** Streams (`subscribe<M>`). */
export type StreamMethod = {
  [M in MethodName]: KindOf<M> extends "stream" ? M : never;
}[MethodName];

export type ParamsOf<M extends MethodName> = ApiMethods[M]["params"];
export type ResultOf<M extends MethodName> = ApiMethods[M]["result"];
export type JobResultOf<M extends JobMethod> = ApiMethods[M] extends { job_result: infer R }
  ? R
  : never;
export type ItemOf<M extends StreamMethod> = ApiMethods[M] extends { item: infer I } ? I : never;

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const ULID_RE = /^[0-9A-HJKMNP-TV-Z]{26}$/;

/** A ULID: 48-bit milliseconds + 80 random bits, Crockford base32 (26 chars). */
export function ulid(
  now: number = Date.now(),
  fill: (bytes: Uint8Array<ArrayBuffer>) => void = (bytes) => crypto.getRandomValues(bytes),
): string {
  let time = "";
  let rest = Math.max(0, Math.floor(now));
  for (let i = 0; i < 10; i += 1) {
    time = CROCKFORD.charAt(rest % 32) + time;
    rest = Math.floor(rest / 32);
  }
  const bytes = new Uint8Array(16);
  fill(bytes);
  let random = "";
  for (const byte of bytes) random += CROCKFORD.charAt(byte % 32);
  return time + random;
}

export function isUlid(text: string): boolean {
  return ULID_RE.test(text);
}

/** One user action ("Install PHP 8.4"): its `request_id` is reused on every retry. */
export interface Intent {
  readonly requestId: string;
}

export function newIntent(): Intent {
  return { requestId: ulid() };
}

export interface CallOptions {
  /** The user action this call belongs to (retries pass the same intent). */
  intent?: Intent;
  /** Explicit `request_id` (wins over `intent`). */
  requestId?: string;
  timeoutMs?: number;
}

export interface JobOptions extends CallOptions {
  /** `job.progress` of this job. */
  onProgress?: (progress: T.JobProgress) => void;
  /** Aborting asks the daemon to cancel (`jobs.cancel`); `done` then rejects `cancelled`. */
  signal?: AbortSignal;
}

/** A started job: its first snapshot and the outcome. */
export interface JobHandle<R> {
  job: T.JobDto;
  /** Resolves with `job.done.result`; rejects with the job's error (`ForgeApiError`). */
  done: Promise<R>;
}

export interface ForgeApiOptions {
  /** Safety poll of `jobs.get` while following a job. */
  jobPollMs?: number;
}

const JOB_EVENT_KINDS = ["job.*"];
const TERMINAL = new Set<T.JobState>(["done", "failed", "cancelled"]);

function cancelledError(job: string, origin: { method: string; requestId: string }): ForgeApiError {
  return new ForgeApiError(
    {
      code: "cancelled",
      message: `${job} was cancelled`,
      details: { reason: "job_cancelled", args: { job } },
    },
    origin,
  );
}

/** The typed API of one context. */
export class ForgeApi {
  readonly bridge: Bridge;
  readonly context: ContextId;
  private readonly jobPollMs: number;

  constructor(bridge: Bridge, context: ContextId = LOCAL_CONTEXT, options: ForgeApiOptions = {}) {
    this.bridge = bridge;
    this.context = context;
    this.jobPollMs = options.jobPollMs ?? 1000;
  }

  /** The same API on another context. */
  withContext(context: ContextId): ForgeApi {
    return new ForgeApi(this.bridge, context, { jobPollMs: this.jobPollMs });
  }

  state(): Promise<ContextStateDto> {
    return this.bridge.state(this.context);
  }

  start(options?: StartOptions): Promise<ContextStateDto> {
    return this.bridge.start(this.context, options);
  }

  /** Calls `method`; rejects with `ForgeApiError`. */
  async call<M extends CallMethod>(
    method: M,
    params: ParamsOf<M>,
    options: CallOptions = {},
  ): Promise<ResultOf<M>> {
    const requestId = options.requestId ?? options.intent?.requestId ?? ulid();
    try {
      const result = await this.bridge.call({
        context: this.context,
        method,
        params,
        request_id: requestId,
        ...(options.timeoutMs === undefined ? {} : { timeout_ms: options.timeoutMs }),
      });
      return result as ResultOf<M>;
    } catch (error) {
      throw toForgeApiError(error, { method, requestId });
    }
  }

  /** Starts a job and follows it to its end. */
  async job<M extends JobMethod>(
    method: M,
    params: ParamsOf<M>,
    options: JobOptions = {},
  ): Promise<JobHandle<JobResultOf<M>>> {
    const requestId = options.requestId ?? options.intent?.requestId ?? ulid();
    const origin = { method, requestId };
    const follower = new JobFollower(this, origin, this.jobPollMs, options.onProgress);
    // Subscribed before the call: a job that ends before its answer is still seen.
    let stream: StreamHandle | null = null;
    try {
      stream = await this.bridge.subscribe(
        this.context,
        "events.subscribe",
        { filter: { kinds: JOB_EVENT_KINDS }, resume_from: null },
        (frame) => {
          follower.onFrame(frame);
        },
      );
    } catch {
      stream = null; // The poll still follows the job.
    }
    let job: T.JobDto;
    try {
      job = await this.call(method, params, { ...options, requestId });
    } catch (error) {
      void stream?.unsubscribe().catch(() => undefined);
      throw error;
    }
    const done = follower.follow(job, options.signal) as Promise<JobResultOf<M>>;
    const cleanup = () => {
      void stream?.unsubscribe().catch(() => undefined);
    };
    done.then(cleanup, cleanup);
    return { job, done };
  }

  /** Opens a stream of `method`; `onFrame` gets each frame's entries in order. */
  subscribe<M extends StreamMethod>(
    method: M,
    params: ParamsOf<M>,
    onFrame: (entries: StreamEntry<ItemOf<M>>[], frame: StreamFrame<ItemOf<M>>) => void,
  ): Promise<StreamHandle> {
    return this.bridge
      .subscribe(this.context, method, params, (frame) => {
        const typed = frame as StreamFrame<ItemOf<M>>;
        onFrame(typed.entries, typed);
      })
      .catch((error: unknown) => {
        throw toForgeApiError(error, { method });
      });
  }
}

/** Follows one job through `job.*` events plus the `jobs.get` safety poll. */
class JobFollower {
  private readonly api: ForgeApi;
  private readonly origin: { method: string; requestId: string };
  private readonly pollMs: number;
  private readonly onProgress: ((progress: T.JobProgress) => void) | undefined;
  /** Events seen before the job id was known (keyed by job id). */
  private readonly early = new Map<string, T.EventDto[]>();
  private jobId: string | null = null;
  private settle:
    ((outcome: { ok: true; value: unknown } | { ok: false; error: ForgeApiError }) => void) | null =
    null;
  private pollNow: (() => void) | null = null;

  constructor(
    api: ForgeApi,
    origin: { method: string; requestId: string },
    pollMs: number,
    onProgress: ((progress: T.JobProgress) => void) | undefined,
  ) {
    this.api = api;
    this.origin = origin;
    this.pollMs = pollMs;
    this.onProgress = onProgress;
  }

  onFrame(frame: StreamFrame): void {
    for (const entry of frame.entries) {
      if (entry.kind === "item") {
        const envelope = entry.item as T.EventEnvelope;
        this.onEvent(envelope.event);
      } else {
        // `lagged`/`reset`/`end`: what we missed is in the snapshot.
        this.pollNow?.();
      }
    }
  }

  private onEvent(event: T.EventDto): void {
    const payload = event.payload as { job_id?: string } | null;
    const jobId = payload?.job_id;
    if (typeof jobId !== "string") return;
    if (this.jobId === null) {
      const list = this.early.get(jobId) ?? [];
      list.push(event);
      this.early.set(jobId, list);
      return;
    }
    if (jobId === this.jobId) this.apply(event);
  }

  private apply(event: T.EventDto): void {
    switch (event.kind) {
      case "job.progress": {
        const payload = event.payload as T.JobProgressPayload;
        const progress: T.JobProgress = {
          pct: payload.pct ?? null,
          message: payload.message ?? null,
          bytes_done: payload.bytes_done ?? null,
          bytes_total: payload.bytes_total ?? null,
          speed_bps: payload.speed_bps ?? null,
          eta_secs: payload.eta_secs ?? null,
        };
        this.onProgress?.(progress);
        break;
      }
      case "job.done":
        this.settle?.({ ok: true, value: (event.payload as T.JobDonePayload).result });
        break;
      case "job.failed": {
        const { error } = event.payload as T.JobFailedPayload;
        this.settle?.({ ok: false, error: new ForgeApiError(error, this.origin) });
        break;
      }
      case "job.cancelled":
        this.settle?.({ ok: false, error: cancelledError(this.origin.method, this.origin) });
        break;
      default:
        break;
    }
  }

  private fromSnapshot(job: T.JobDto): void {
    if (!TERMINAL.has(job.state)) {
      this.onProgress?.(job.progress);
      return;
    }
    if (job.state === "done") {
      this.settle?.({ ok: true, value: job.result });
    } else if (job.state === "failed") {
      const error = job.error ?? { code: "internal", message: `${job.name} failed` };
      this.settle?.({ ok: false, error: new ForgeApiError(error, this.origin) });
    } else {
      this.settle?.({ ok: false, error: cancelledError(job.name, this.origin) });
    }
  }

  follow(job: T.JobDto, signal: AbortSignal | undefined): Promise<unknown> {
    return new Promise<unknown>((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      let polling = false;
      // An object: `settle` flips it from the event/snapshot paths called below.
      const run = { finished: false };
      const onAbort = () => {
        void this.api.call("jobs.cancel", { job_id: job.id }).catch(() => undefined);
      };
      this.settle = (outcome) => {
        if (run.finished) return;
        run.finished = true;
        if (timer !== null) clearTimeout(timer);
        signal?.removeEventListener("abort", onAbort);
        this.settle = null;
        this.pollNow = null;
        if (outcome.ok) resolve(outcome.value);
        else reject(outcome.error);
      };
      const poll = async () => {
        if (run.finished || polling) return;
        polling = true;
        try {
          this.fromSnapshot(await this.api.call("jobs.get", { job_id: job.id }));
        } catch (error) {
          const failure = toForgeApiError(error, this.origin);
          // Connection trouble: keep following; anything else ends the wait.
          if (failure.code !== "disconnected" && failure.code !== "timeout") {
            this.settle?.({ ok: false, error: failure });
          }
        } finally {
          polling = false;
        }
      };
      const schedule = () => {
        if (run.finished) return;
        timer = setTimeout(() => {
          void poll().finally(schedule);
        }, this.pollMs);
      };
      this.pollNow = () => {
        void poll();
      };
      this.jobId = job.id;
      this.fromSnapshot(job);
      for (const event of this.early.get(job.id) ?? []) this.apply(event);
      this.early.clear();
      if (run.finished) return;
      if (signal?.aborted) onAbort();
      else signal?.addEventListener("abort", onAbort, { once: true });
      schedule();
    });
  }
}
