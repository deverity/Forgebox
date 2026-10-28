// User actions while the daemon is away (docs/03 §7.5: the GUI stays usable and queues what
// makes sense). A user-triggered mutating call made while the context is reconnecting or
// starting — or that fails with `disconnected` — is held with its `request_id` for up to
// 30 s and sent as soon as the context is `connected` again. The same `request_id` makes the
// resend safe when the first attempt did reach the daemon (it answers instead of acting
// twice, ADR-0009 rule 4). After 30 s the call fails with `disconnected`/`reconnect_timeout`,
// which `describeError` localizes. With the daemon deliberately stopped (or never
// initialized, or the context closed) nothing would bring it back by itself, so calls fail
// at once with `disconnected`/`daemon_stopped`.
// Reads (non-mutating calls) are never held: screens show their disconnected state instead.
import type { StoreApi } from "zustand/vanilla";
import {
  ulid,
  type CallMethod,
  type CallOptions,
  type ForgeApi,
  type JobHandle,
  type JobMethod,
  type JobOptions,
  type JobResultOf,
  type ParamsOf,
  type ResultOf,
} from "../ipc/api";
import type { ConnectionStatus } from "../ipc/bridge";
import { ForgeApiError, apiError, toForgeApiError } from "../ipc/errors";
import { METHODS, type MethodMeta } from "../ipc/methods";
import type { ConnectionSlice } from "./stores";

export const RECONNECT_HOLD_MS = 30_000;

/** Statuses that come back to `connected` by themselves. */
const HOLD_STATUSES = new Set<ConnectionStatus>(["idle", "connecting", "reconnecting", "starting"]);

interface Held {
  method: string;
  requestId: string;
  send: () => Promise<unknown>;
  resolve: (value: unknown) => void;
  reject: (error: ForgeApiError) => void;
  timer: ReturnType<typeof setTimeout>;
  /** A resend is on its way: the deadline waits for its outcome. */
  sending: boolean;
  expired: boolean;
}

function isMutating(method: string): boolean {
  return (METHODS as Record<string, MethodMeta | undefined>)[method]?.mutating === true;
}

export class ReconnectQueue {
  private readonly api: ForgeApi;
  private readonly connection: StoreApi<ConnectionSlice>;
  private readonly holdMs: number;
  private readonly held: Held[] = [];
  private draining = false;
  private readonly unsubscribe: () => void;

  constructor(api: ForgeApi, connection: StoreApi<ConnectionSlice>, holdMs = RECONNECT_HOLD_MS) {
    this.api = api;
    this.connection = connection;
    this.holdMs = holdMs;
    this.unsubscribe = connection.subscribe((state, previous) => {
      if (state.status === "connected" && previous.status !== "connected") void this.drain();
    });
  }

  /** Calls waiting for the connection. */
  get size(): number {
    return this.held.length;
  }

  /** A user-triggered call: sent now, or held while the daemon comes back. */
  call<M extends CallMethod>(
    method: M,
    params: ParamsOf<M>,
    options: CallOptions = {},
  ): Promise<ResultOf<M>> {
    const requestId = options.requestId ?? options.intent?.requestId ?? ulid();
    return this.submit(method, requestId, () =>
      this.api.call(method, params, { ...options, requestId }),
    ) as Promise<ResultOf<M>>;
  }

  /** A user-triggered job: started now, or held while the daemon comes back. */
  job<M extends JobMethod>(
    method: M,
    params: ParamsOf<M>,
    options: JobOptions = {},
  ): Promise<JobHandle<JobResultOf<M>>> {
    const requestId = options.requestId ?? options.intent?.requestId ?? ulid();
    return this.submit(method, requestId, () =>
      this.api.job(method, params, { ...options, requestId }),
    ) as Promise<JobHandle<JobResultOf<M>>>;
  }

  dispose(): void {
    this.unsubscribe();
    for (const entry of this.held.splice(0)) {
      clearTimeout(entry.timer);
      entry.reject(this.error(entry, "context_closed"));
    }
  }

  private async submit(
    method: string,
    requestId: string,
    send: () => Promise<unknown>,
  ): Promise<unknown> {
    if (!isMutating(method)) return send();
    const status = this.connection.getState().status;
    if (status === "connected") {
      try {
        return await send();
      } catch (error) {
        const failure = toForgeApiError(error, { method, requestId });
        if (failure.code !== "disconnected") throw failure;
        return this.hold(method, requestId, send);
      }
    }
    if (HOLD_STATUSES.has(status)) return this.hold(method, requestId, send);
    throw this.error({ method, requestId }, "daemon_stopped");
  }

  private hold(method: string, requestId: string, send: () => Promise<unknown>): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const entry: Held = {
        method,
        requestId,
        send,
        resolve,
        reject,
        sending: false,
        expired: false,
        timer: setTimeout(() => {
          entry.expired = true;
          // A resend in flight decides (it may have reached the daemon).
          if (entry.sending) return;
          this.settle(entry);
          reject(this.error(entry, "reconnect_timeout"));
        }, this.holdMs),
      };
      this.held.push(entry);
      // Connected again while the failing attempt was on its way back.
      if (this.connection.getState().status === "connected") void this.drain();
    });
  }

  /** Sends the held calls in order, each with its own `request_id`. */
  private async drain(): Promise<void> {
    if (this.draining) return;
    this.draining = true;
    try {
      while (this.held.length > 0 && this.connection.getState().status === "connected") {
        const entry = this.held[0];
        if (entry === undefined) break;
        entry.sending = true;
        try {
          const value = await entry.send();
          this.settle(entry);
          entry.resolve(value);
        } catch (error) {
          const failure = toForgeApiError(error, {
            method: entry.method,
            requestId: entry.requestId,
          });
          if (failure.code === "disconnected") {
            if (entry.expired) {
              this.settle(entry);
              entry.reject(this.error(entry, "reconnect_timeout"));
              continue;
            }
            // Dropped again: keep it (its deadline still runs) until the next connection.
            break;
          }
          this.settle(entry);
          entry.reject(failure);
        } finally {
          entry.sending = false;
        }
      }
    } finally {
      this.draining = false;
    }
  }

  private settle(entry: Held): void {
    clearTimeout(entry.timer);
    const index = this.held.indexOf(entry);
    if (index >= 0) this.held.splice(index, 1);
  }

  private error(
    entry: Pick<Held, "method" | "requestId">,
    reason: "reconnect_timeout" | "daemon_stopped" | "context_closed",
  ): ForgeApiError {
    const message =
      reason === "reconnect_timeout"
        ? `the daemon did not come back within ${String(this.holdMs / 1000)} s; ${entry.method} was not sent`
        : `the daemon is not running; ${entry.method} was not sent`;
    return new ForgeApiError(
      apiError("disconnected", message, reason, {
        method: entry.method,
        after_ms: this.holdMs,
      }),
      { method: entry.method, requestId: entry.requestId },
    );
  }
}
