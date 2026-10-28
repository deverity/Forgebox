// React bindings of the state layer. Components read the stores through selectors (one
// entity per subscription, so an event for another entity re-renders nothing here) and get
// the API, jobs, metrics and capabilities of the active context.
import { QueryClientProvider, useQuery, type UseQueryResult } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { useStore } from "zustand";
import type { CallMethod, ForgeApi, ParamsOf, ResultOf } from "../ipc/api";
import { LOCAL_CONTEXT, type ContextId } from "../ipc/bridge";
import type { ForgeApiError } from "../ipc/errors";
import type * as T from "../ipc/generated";
import { capabilityOf, type Capability } from "./capabilities";
import { cancelJob, isTerminal, jobError, watchJob } from "./jobs";
import type { StateLayer } from "./layer";
import type { MetricsConsumer } from "./metrics";
import { forgeQuery } from "./queries";
import type { ContextRuntime } from "./runtime";
import type {
  AttentionSlice,
  ConnectionSlice,
  InstanceSample,
  PoolSample,
  StatusTotals,
  SyncSlice,
  SystemSlice,
  ToolsSlice,
} from "./stores";

const RuntimeContext = createContext<ContextRuntime | null>(null);

export interface ForgeStateProviderProps {
  layer: StateLayer;
  /** Active context (`local` in M2). */
  context?: ContextId;
  children: ReactNode;
}

/**
 * Provides the runtime of `context` and the shared query client. The runtime starts in an
 * effect, i.e. after the first commit: the shell paints before any daemon traffic.
 */
export function ForgeStateProvider({
  layer,
  context = LOCAL_CONTEXT,
  children,
}: ForgeStateProviderProps) {
  const runtime = layer.runtime(context);
  useEffect(() => {
    void runtime.start();
  }, [runtime]);
  return (
    <QueryClientProvider client={layer.queryClient}>
      <RuntimeContext.Provider value={runtime}>{children}</RuntimeContext.Provider>
    </QueryClientProvider>
  );
}

export function useRuntime(): ContextRuntime {
  const runtime = useContext(RuntimeContext);
  if (runtime === null) throw new Error("useRuntime outside <ForgeStateProvider>");
  return runtime;
}

export function useForgeApi(): ForgeApi {
  return useRuntime().api;
}

// --- stores ------------------------------------------------------------------------------

export function useConnection<R>(selector: (state: ConnectionSlice) => R): R {
  return useStore(useRuntime().stores.connection, selector);
}

export function useConnectionStatus(): ConnectionSlice["status"] {
  return useConnection((state) => state.status);
}

export function useHello(): T.HelloDto | null {
  return useConnection((state) => state.hello);
}

export function useInstance(id: string): T.InstanceDto | undefined {
  return useStore(useRuntime().stores.services, (state) => state.instances[id]);
}

export function useInstances(): Readonly<Record<string, T.InstanceDto>> {
  return useStore(useRuntime().stores.services, (state) => state.instances);
}

export function usePool(versionId: string): T.PoolDto | undefined {
  return useStore(useRuntime().stores.services, (state) => state.pools[versionId]);
}

export function usePools(): Readonly<Record<string, T.PoolDto>> {
  return useStore(useRuntime().stores.services, (state) => state.pools);
}

export function useSite(id: string): T.SiteDto | undefined {
  return useStore(useRuntime().stores.sites, (state) => state.sites[id]);
}

export function useSites(): Readonly<Record<string, T.SiteDto>> {
  return useStore(useRuntime().stores.sites, (state) => state.sites);
}

export function useTools<R>(selector: (state: ToolsSlice) => R): R {
  return useStore(useRuntime().stores.tools, selector);
}

/** `tool.updates_available`: the pending list and its count (sidebar badge). */
export function useToolUpdates(): { updates: ToolsSlice["updates"]; count: number } {
  const updates = useTools((state) => state.updates);
  const count = useTools((state) => state.updateCount);
  return useMemo(() => ({ updates, count }), [updates, count]);
}

export function useSystem<R>(selector: (state: SystemSlice) => R): R {
  return useStore(useRuntime().stores.system, selector);
}

export function useAttention<R>(selector: (state: AttentionSlice) => R): R {
  return useStore(useRuntime().stores.attention, selector);
}

export function useSyncState<R>(selector: (state: SyncSlice) => R): R {
  return useStore(useRuntime().stores.sync, selector);
}

// --- queries -----------------------------------------------------------------------------

/** A cached call of the active context, invalidated by the events that make it stale. */
export function useForgeQuery<M extends CallMethod>(
  method: M,
  params: ParamsOf<M>,
  options: { enabled?: boolean } = {},
): UseQueryResult<ResultOf<M>, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability(method);
  return useQuery({
    ...forgeQuery(api, method, params),
    enabled: (options.enabled ?? true) && capability.available,
  });
}

// --- capabilities ------------------------------------------------------------------------

export interface Capabilities {
  hello: T.HelloDto | null;
  can(method: string): Capability;
}

export function useCapabilities(): Capabilities {
  const hello = useHello();
  return useMemo(
    () => ({
      hello,
      can: (method: string) => capabilityOf(method, hello),
    }),
    [hello],
  );
}

export function useCapability(method: string): Capability {
  const hello = useHello();
  return useMemo(() => capabilityOf(method, hello), [method, hello]);
}

// --- jobs --------------------------------------------------------------------------------

export interface JobView {
  job: T.JobDto | undefined;
  state: T.JobState | "unknown";
  progress: T.JobProgress;
  result: unknown;
  /** The job's failure (`cancelled` included) or a failed safety poll. */
  error: ForgeApiError | null;
  running: boolean;
  cancel: () => Promise<void>;
}

const NO_PROGRESS: T.JobProgress = {};

/**
 * Follows `jobId`: the `job.*` events through the jobs store plus a `jobs.get` safety poll
 * every second until the job ends; `cancel()` asks the daemon to cancel.
 */
export function useJob(jobId: string | null): JobView {
  const runtime = useRuntime();
  const job = useStore(runtime.stores.jobs, (state) =>
    jobId === null ? undefined : state.jobs[jobId],
  );
  const [pollError, setPollError] = useState<ForgeApiError | null>(null);
  const terminal = isTerminal(job);
  useEffect(() => {
    if (jobId === null || terminal) return undefined;
    return watchJob(jobId, {
      api: runtime.api,
      store: runtime.stores.jobs,
      pollMs: runtime.jobPollMs,
      onError: setPollError,
    });
  }, [runtime, jobId, terminal]);
  const cancel = useCallback(async () => {
    if (jobId === null) return;
    await cancelJob(runtime.queue, runtime.stores.jobs, jobId);
  }, [runtime, jobId]);
  return useMemo(
    () => ({
      job,
      state: job?.state ?? "unknown",
      progress: job?.progress ?? NO_PROGRESS,
      result: job?.state === "done" ? job.result : null,
      error: jobError(job) ?? pollError,
      running: job?.state === "running",
      cancel,
    }),
    [job, pollError, cancel],
  );
}

// --- metrics -----------------------------------------------------------------------------

/**
 * Registers the component as a metrics consumer: `services.status` (and `daemon.status`
 * with `{daemon: true}`) is polled every 2 s while any consumer is mounted and the window is
 * visible and focused.
 */
export function useMetricsPoll(consumer: MetricsConsumer = {}): void {
  const runtime = useRuntime();
  const daemon = consumer.daemon === true;
  useEffect(() => runtime.metrics.acquire({ daemon }), [runtime, daemon]);
}

const NO_SAMPLES: readonly never[] = [];

/** The last 30 samples (60 s) of an instance; polls while mounted. */
export function useInstanceMetrics(id: string): readonly InstanceSample[] {
  useMetricsPoll();
  return useStore(useRuntime().stores.metrics, (state) => state.instances[id] ?? NO_SAMPLES);
}

/** The last 30 samples of a pool (req/s from `requests_total`); polls while mounted. */
export function usePoolMetrics(versionId: string): readonly PoolSample[] {
  useMetricsPoll();
  return useStore(useRuntime().stores.metrics, (state) => state.pools[versionId] ?? NO_SAMPLES);
}

/** Status-bar totals (daemon cpu/rss + the stack); polls while mounted. */
export function useStatusTotals(): StatusTotals | null {
  useMetricsPoll({ daemon: true });
  return useStore(useRuntime().stores.metrics, (state) => state.totals);
}
