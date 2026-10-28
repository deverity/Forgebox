// The frontend data layer (M2-E1-T9, ADR-0034 "camada de estado"): stores, events, queries,
// jobs, metrics, capabilities and the reconnect queue of each context. Screens import from
// here; nothing in it knows a tool.
export {
  capabilityOf,
  capabilityTable,
  type Capability,
  type CapabilityReason,
} from "./capabilities";
export { EventStream, type GapReason } from "./eventStream";
export {
  FrameBatcher,
  browserFrameClock,
  manualFrameClock,
  type FrameClock,
  type ManualFrameClock,
} from "./frame";
export { cancelJob, isTerminal, jobError, upsertJob, watchJob } from "./jobs";
export { StateLayer } from "./layer";
export {
  METRICS_INTERVAL_MS,
  METRICS_SAMPLES,
  MetricsPoller,
  ingestMetrics,
  requestRate,
  statusTotals,
  type MetricsConsumer,
} from "./metrics";
export {
  browserPageActivity,
  manualPageActivity,
  type ManualPageActivity,
  type PageActivity,
} from "./page";
export { EVENT_INVALIDATIONS, Invalidator, forgeQuery, queryKeys } from "./queries";
export {
  ForgeStateProvider,
  useAttention,
  useCapabilities,
  useCapability,
  useConnection,
  useConnectionStatus,
  useForgeApi,
  useForgeQuery,
  useHello,
  useInstance,
  useInstanceMetrics,
  useInstances,
  useJob,
  useMetricsPoll,
  usePool,
  usePoolMetrics,
  usePools,
  useRuntime,
  useSite,
  useSites,
  useStatusTotals,
  useSyncState,
  useSystem,
  useToolUpdates,
  useTools,
  type Capabilities,
  type JobView,
} from "./react";
export { RECONNECT_HOLD_MS, ReconnectQueue } from "./reconnectQueue";
export { EVENT_REDUCERS, isIgnored, type Ignored, type ReducerRegistry, type Tx } from "./reducers";
export { ContextRuntime, type RuntimeOptions } from "./runtime";
export { SNAPSHOTS } from "./snapshots";
export type * from "./stores";
