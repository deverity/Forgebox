// Following a job (docs/03 §7.4, ADR-0030): the `job.*` reducers keep the jobs store
// current from events; `watchJob` adds the `jobs.get` safety poll (every second until the
// job ends, so a dropped event never leaves a job "running" forever) and `cancelJob` asks
// the daemon to cancel (the job then ends `cancelled` by event or by poll).
import type { StoreApi } from "zustand/vanilla";
import type { ForgeApi } from "../ipc/api";
import { ForgeApiError, toForgeApiError } from "../ipc/errors";
import type * as T from "../ipc/generated";
import type { ReconnectQueue } from "./reconnectQueue";
import { deepEqual, type JobsSlice } from "./stores";

const TERMINAL = new Set<T.JobState>(["done", "failed", "cancelled"]);

export function isTerminal(job: T.JobDto | undefined): boolean {
  return job !== undefined && TERMINAL.has(job.state);
}

/** Writes a `jobs.get` snapshot: a terminal state seen first is never undone. */
export function upsertJob(store: StoreApi<JobsSlice>, job: T.JobDto): void {
  const current = store.getState().jobs[job.id];
  if (current !== undefined) {
    if (TERMINAL.has(current.state) && job.state === "running") return;
    if (deepEqual(current, job)) return;
  }
  store.setState((state) => ({ jobs: { ...state.jobs, [job.id]: job } }));
}

export interface WatchJobOptions {
  api: ForgeApi;
  store: StoreApi<JobsSlice>;
  pollMs: number;
  /** The poll failed for a reason other than the connection (`not_found`...). */
  onError?: (error: ForgeApiError) => void;
}

/**
 * Polls `jobs.get` every `pollMs` until the job is terminal in the store (by event or by
 * poll). Connection errors keep polling. Returns the stop function.
 */
export function watchJob(jobId: string, options: WatchJobOptions): () => void {
  const { api, store, pollMs } = options;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;
  // A function, not the variable: the stop function flips it while a poll awaits.
  const isStopped = () => stopped;
  const done = () => isTerminal(store.getState().jobs[jobId]);
  const schedule = () => {
    if (isStopped() || done()) return;
    timer = setTimeout(() => {
      timer = null;
      void poll();
    }, pollMs);
  };
  const poll = async () => {
    if (isStopped() || done()) return;
    try {
      const job = await api.call("jobs.get", { job_id: jobId });
      if (!isStopped()) upsertJob(store, job);
    } catch (error) {
      const failure = toForgeApiError(error, { method: "jobs.get" });
      if (failure.code !== "disconnected" && failure.code !== "timeout") {
        options.onError?.(failure);
        stopped = true;
        return;
      }
    }
    schedule();
  };
  // Stop as soon as an event ends the job.
  const unsubscribe = store.subscribe((state) => {
    if (isTerminal(state.jobs[jobId]) && timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  });
  schedule();
  return () => {
    stopped = true;
    if (timer !== null) clearTimeout(timer);
    timer = null;
    unsubscribe();
  };
}

/** Asks the daemon to cancel; the job ends `cancelled` through its event or the poll. */
export async function cancelJob(
  queue: ReconnectQueue,
  store: StoreApi<JobsSlice>,
  jobId: string,
): Promise<T.JobCancelDto> {
  const current = store.getState().jobs[jobId];
  if (current?.state === "running" && !current.cancel_requested) {
    store.setState((state) => ({
      jobs: { ...state.jobs, [jobId]: { ...current, cancel_requested: true } },
    }));
  }
  return queue.call("jobs.cancel", { job_id: jobId });
}

/** The job's error as a `ForgeApiError` (`cancelled` for a cancelled job). */
export function jobError(job: T.JobDto | undefined): ForgeApiError | null {
  if (job === undefined) return null;
  const origin = { method: job.name, requestId: job.request_id ?? undefined };
  if (job.state === "failed") {
    return new ForgeApiError(
      job.error ?? { code: "internal", message: `${job.name} failed` },
      origin,
    );
  }
  if (job.state === "cancelled") {
    return new ForgeApiError(
      {
        code: "cancelled",
        message: `${job.name} was cancelled`,
        details: { reason: "job_cancelled", args: { job: job.name } },
      },
      origin,
    );
  }
  return null;
}
