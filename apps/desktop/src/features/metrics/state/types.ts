import type { ForgeApiError } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";

/** One chart point kept per sample (the full `SiteStatsDto` is only kept for the latest one). */
export interface MetricsPoint {
  /** Epoch ms of `SiteStatsDto.at`. */
  t: number;
  requests: number;
  rps: number | null;
  p50: number | null;
  p90: number | null;
  p99: number | null;
  errorPct: number | null;
  cpu: number | null;
  mem: number | null;
}

/**
 * State of one (site, window) store.
 * `idle`: nobody subscribed or the daemon cannot stream; `pending`: waiting for the first
 * sample; `live`: at least one sample arrived; `failed`: the subscription was refused or ended.
 */
export interface SiteMetricsState {
  status: "idle" | "pending" | "live" | "failed";
  latest: T.SiteStatsDto | null;
  /** Oldest first, strictly increasing in `t`, at most `MAX_POINTS`. */
  points: readonly MetricsPoint[];
  /** Items the stream reported as lost (`metrics.lagged`) since the snapshot. */
  missed: number;
  /** Times a snapshot replaced the store (first sample after a reconnect or a `reset`). */
  snapshots: number;
  error: ForgeApiError | null;
}

export const MAX_POINTS = 300;

export const EMPTY_METRICS: SiteMetricsState = {
  status: "idle",
  latest: null,
  points: [],
  missed: 0,
  snapshots: 0,
  error: null,
};
