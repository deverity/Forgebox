// The IPC bridge contract between the webview and src-tauri/src/bridge (ADR-0034).
//
// The webview never talks to a daemon and never sees the API token. It names a context
// ("local" in M2; remote profiles arrive in M6) and goes through a `Bridge`:
// - `tauriBridge` in the app (generic Tauri commands `ipc_*` + the `forge://connection`
//   event, frames over a Tauri Channel);
// - `fakeBridge` in tests and QA builds (the same semantics, scripted per scenario).
// Feature code uses the typed client of `api.ts`, not a `Bridge` directly.
import type { ApiErrorDto } from "./errors";
import type { HelloDto } from "./generated";

export type ContextId = string;

/** The daemon of this machine. */
export const LOCAL_CONTEXT: ContextId = "local";

/** Tauri event carrying every `ContextStateDto` change. */
export const CONNECTION_EVENT = "forge://connection";

/**
 * Where a context stands:
 * - `uninitialized`: the root has no `state/` (onboarding; never started automatically);
 * - `starting`: the bridge is starting the daemon;
 * - `reconnecting`: lost; the client reconnects and may restart the daemon (at most once per 60 s);
 * - `stopped`: the daemon stopped on purpose (`autostart.stopped_reason`); only `start` brings it back.
 */
export type ConnectionStatus =
  | "idle"
  | "uninitialized"
  | "starting"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "stopped"
  | "closed";

export interface AutoStartDto {
  initialized: boolean;
  /** `user`, `signal`, `exited` (graceful exit, announcement lost)... while it holds. */
  stopped_reason: string | null;
  /** Milliseconds until the next automatic restart may run, when one is scheduled. */
  next_restart_ms: number | null;
  /** Automatic restarts done by this app. */
  restarts: number;
}

/** `ipc_state` / `forge://connection` (src-tauri `ContextStateDto`). */
export interface ContextStateDto {
  context: ContextId;
  status: ConnectionStatus;
  /** Grows with every change of this context: keep the highest. */
  version: number;
  hello: HelloDto | null;
  generation: number | null;
  attempt: number;
  next_retry_ms: number;
  last_error: ApiErrorDto | null;
  /** What the context points at, for display (the root folder for `local`). */
  endpoint: string | null;
  autostart: AutoStartDto;
}

/** One entry of a stream frame, in stream order. */
export type StreamEntry<T = unknown> =
  /** The notification params as the daemon sent them (`{subscription_id, seq, ...}`). */
  | { kind: "item"; item: T }
  /** `missed` items were lost exactly here (slow webview, slow link or daemon-side lag). */
  | { kind: "lagged"; missed: number }
  /** The daemon's sequence restarted: refetch snapshots. */
  | { kind: "reset" }
  /** Last entry of a stream that ended by itself (`error` says why when it failed). */
  | { kind: "end"; error: ApiErrorDto | null };

/** What one Channel message carries: ≤ 256 entries, ≤ 16 ms after its first one. */
export interface StreamFrame<T = unknown> {
  stream_id: string;
  /** 1, 2, 3...; acknowledged after the handler returns (flow control). */
  frame: number;
  entries: StreamEntry<T>[];
}

/** Arguments of `ipc_call`. */
export interface CallRequest {
  context: ContextId;
  method: string;
  params?: unknown;
  /** ULID per user intent, reused on retries (ADR-0009 rule 4). */
  request_id: string;
  timeout_ms?: number;
}

/** Answer of `ipc_subscribe`. */
export interface SubscribeAnswer {
  stream_id: string;
  method: string;
  resume_point: number | null;
  /** The daemon was not reachable: the stream subscribes when it comes back. */
  pending: boolean;
}

export interface StreamHandle {
  readonly id: string;
  readonly method: string;
  readonly resumePoint: number | null;
  readonly pending: boolean;
  unsubscribe(): Promise<void>;
}

export type Unlisten = () => void;

export interface StartOptions {
  /** Also on a root that was never initialized (the user finished onboarding). */
  initialize?: boolean;
}

/**
 * Transport between the webview and the daemons. Every method rejects with a
 * `ForgeApiError` (see `errors.ts`).
 */
export interface Bridge {
  /** State of `context` (opens it on first use; connecting happens in the background). */
  state(context: ContextId): Promise<ContextStateDto>;
  /** One API call (validated against `METHODS` before anything is sent). */
  call(request: CallRequest): Promise<unknown>;
  /** Opens a stream; `onFrame` gets every frame in order. */
  subscribe(
    context: ContextId,
    method: string,
    params: unknown,
    onFrame: (frame: StreamFrame) => void,
  ): Promise<StreamHandle>;
  /** The user wants the daemon running (never rate limited). */
  start(context: ContextId, options?: StartOptions): Promise<ContextStateDto>;
  /** Every state change of every context. */
  onConnection(listener: (state: ContextStateDto) => void): Promise<Unlisten>;
}
