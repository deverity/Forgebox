// Zustand stores of one context (ADR-0034, state layer): plain projections of DTOs and
// events, keyed by id (ULIDs; tools by their slug), with no domain logic. One store per
// concern so a component subscribed to one entity never re-renders for another, and each
// event batch commits at most once per store (`Draft`).
import { createStore, type StoreApi } from "zustand/vanilla";
import type { ConnectionStatus, ContextId, ContextStateDto } from "../ipc/bridge";
import type * as T from "../ipc/generated";

export type ById<V> = Readonly<Record<string, V>>;

export interface ConnectionSlice {
  context: ContextId;
  status: ConnectionStatus;
  /** The last `ContextStateDto` (highest `version`). */
  state: ContextStateDto | null;
  hello: T.HelloDto | null;
  generation: number | null;
  /** Round trip of the last `system.ping`, ms. */
  rttMs: number | null;
  /** `daemon.ready.version` of the daemon seen last. */
  daemonVersion: string | null;
  /** `daemon.shutdown.reason` while it holds (cleared on the next connection). */
  shutdownReason: string | null;
}

export interface ServicesSlice {
  instances: ById<T.InstanceDto>;
  /** PHP pools by `version_id`. */
  pools: ById<T.PoolDto>;
  loaded: boolean;
}

export interface SitesSlice {
  sites: ById<T.SiteDto>;
  loaded: boolean;
}

/** A tool install seen through `tool.install.*`. */
export interface ToolInstallEntry {
  key: string;
  tool: string;
  version: string;
  jobId: string | null;
  state: "installing" | "failed";
  step: string | null;
  progress: T.JobProgress | null;
  error: T.JobErrorInfo | null;
  ts: string;
}

export interface ToolsSlice {
  /** Installed tools (`tools.list {installed: true}`) by tool id. */
  tools: ById<T.ToolDto>;
  /** `tool.updates_available.updates`: always the whole pending list. */
  updates: readonly T.PendingUpdateDto[];
  updateCount: number;
  installs: ById<ToolInstallEntry>;
  loaded: boolean;
}

export interface JobsSlice {
  jobs: ById<T.JobDto>;
}

export interface SystemSlice {
  daemon: T.DaemonStatusDto | null;
  dns: T.DnsStatusDto | null;
  ca: T.CaStatusDto | null;
  /** `dns.policy_applied` by TLD. */
  dnsPolicies: ById<T.DnsPolicyPayload>;
  /** Leaf certificates issued or renewed (`tls.leaf_issued|renewed`) by host. */
  certificates: ById<T.TlsPayload & { ts: string }>;
}

type Stamped<P> = P & { ts: string };

/** Event-derived inputs of the Atenção registry (docs/07 §5.2); screens decide what to show. */
export interface AttentionSlice {
  /** `port.conflict` by `<protocol>/<port>` until the owner runs again. */
  portConflicts: ById<Stamped<T.PortConflictPayload>>;
  /** `config.drift_detected` by path until the file is rendered again. */
  configDrift: ById<Stamped<T.ConfigFilePayload>>;
  /** Last `service.failed|crashed` of an instance until it runs or stops again. */
  serviceFailures: ById<Stamped<T.ServicePayload>>;
  /** `actor.failed` by actor. */
  actorFailures: ById<Stamped<T.ActorFailedPayload>>;
  /** `fs.case_sensitivity_unavailable` by dir until applied. */
  caseUnavailable: ById<Stamped<T.FsCaseSensitivityPayload>>;
  /** Failed `helper.op` by operation name until it succeeds. */
  helperFailures: ById<Stamped<T.HelperOpPayload>>;
}

export interface InstanceSample {
  /** Client time of the poll (ms). */
  t: number;
  cpu: number | null;
  rss: number | null;
}

export interface PoolSample {
  t: number;
  /** Requests per second since the previous sample (`requests_total` delta). */
  rps: number | null;
  requests: number | null;
  busy: number;
  workers: number;
  rss: number | null;
}

/** What the status bar shows (docs/07 §4): the daemon itself plus the stack. */
export interface StatusTotals {
  daemonCpu: number | null;
  daemonRss: number | null;
  /** Sum over instances (pools report no CPU). */
  stackCpu: number;
  /** Sum over instances and pools. */
  stackRss: number;
  cpu: number;
  rss: number;
  running: number;
}

export interface MetricsSlice {
  instances: ById<readonly InstanceSample[]>;
  pools: ById<readonly PoolSample[]>;
  totals: StatusTotals | null;
  lastPollAt: number | null;
}

export type SliceName = "services" | "sites" | "tools" | "jobs" | "dns" | "ca" | "daemon";

export interface SyncSlice {
  /** Completed full resyncs (snapshots refetched after a gap, a reset or a restart). */
  resyncs: number;
  lastResyncReason: string | null;
  loading: Readonly<Partial<Record<SliceName, boolean>>>;
  errors: Readonly<Partial<Record<SliceName, T.ApiError | null>>>;
  /** Highest event `seq` received. */
  cursor: number;
}

export interface ContextStores {
  connection: StoreApi<ConnectionSlice>;
  services: StoreApi<ServicesSlice>;
  sites: StoreApi<SitesSlice>;
  tools: StoreApi<ToolsSlice>;
  jobs: StoreApi<JobsSlice>;
  system: StoreApi<SystemSlice>;
  attention: StoreApi<AttentionSlice>;
  metrics: StoreApi<MetricsSlice>;
  sync: StoreApi<SyncSlice>;
}

/** Stores whose state is written by event batches (`Tx`). */
export type EventStoreName =
  "connection" | "services" | "sites" | "tools" | "jobs" | "system" | "attention";

export function createContextStores(context: ContextId): ContextStores {
  return {
    connection: createStore<ConnectionSlice>(() => ({
      context,
      status: "idle",
      state: null,
      hello: null,
      generation: null,
      rttMs: null,
      daemonVersion: null,
      shutdownReason: null,
    })),
    services: createStore<ServicesSlice>(() => ({ instances: {}, pools: {}, loaded: false })),
    sites: createStore<SitesSlice>(() => ({ sites: {}, loaded: false })),
    tools: createStore<ToolsSlice>(() => ({
      tools: {},
      updates: [],
      updateCount: 0,
      installs: {},
      loaded: false,
    })),
    jobs: createStore<JobsSlice>(() => ({ jobs: {} })),
    system: createStore<SystemSlice>(() => ({
      daemon: null,
      dns: null,
      ca: null,
      dnsPolicies: {},
      certificates: {},
    })),
    attention: createStore<AttentionSlice>(() => ({
      portConflicts: {},
      configDrift: {},
      serviceFailures: {},
      actorFailures: {},
      caseUnavailable: {},
      helperFailures: {},
    })),
    metrics: createStore<MetricsSlice>(() => ({
      instances: {},
      pools: {},
      totals: null,
      lastPollAt: null,
    })),
    sync: createStore<SyncSlice>(() => ({
      resyncs: 0,
      lastResyncReason: null,
      loading: {},
      errors: {},
      cursor: 0,
    })),
  };
}

type MapKeys<S> = {
  [K in keyof S]: S[K] extends ById<unknown> ? K : never;
}[keyof S];
type ValueOf<S, K extends keyof S> = S[K] extends ById<infer V> ? V : never;

/**
 * Copy-on-write working copy of one store for the length of a batch: the first write copies
 * the state (and the first write to a map copies that map), unchanged entries keep their
 * identity, and `commit()` calls `setState` once if anything changed.
 */
export class Draft<S extends object> {
  private next: S | null = null;
  private readonly copied = new Set<keyof S>();
  private readonly store: StoreApi<S>;

  constructor(store: StoreApi<S>) {
    this.store = store;
  }

  get state(): S {
    return this.next ?? this.store.getState();
  }

  get changed(): boolean {
    return this.next !== null;
  }

  get<K extends MapKeys<S>>(field: K, id: string): ValueOf<S, K> | undefined {
    const map = this.state[field] as Record<string, ValueOf<S, K>>;
    return Object.hasOwn(map, id) ? map[id] : undefined;
  }

  put<K extends MapKeys<S>>(field: K, id: string, value: ValueOf<S, K>): void {
    if (this.get(field, id) === value) return;
    this.mapForWrite(field)[id] = value;
  }

  remove(field: MapKeys<S>, id: string): void {
    if (this.get(field, id) === undefined) return;
    // eslint-disable-next-line @typescript-eslint/no-dynamic-delete -- id-keyed map
    delete this.mapForWrite(field)[id];
  }

  /** Removes every entry of `field` matching `predicate`. */
  removeWhere<K extends MapKeys<S>>(
    field: K,
    predicate: (value: ValueOf<S, K>, id: string) => boolean,
  ): void {
    const map = this.state[field] as Record<string, ValueOf<S, K>>;
    for (const [id, value] of Object.entries(map)) {
      if (predicate(value, id)) this.remove(field, id);
    }
  }

  patch(partial: Partial<S>): void {
    const current = this.state;
    const differs = (Object.keys(partial) as (keyof S)[]).some(
      (key) => !Object.is(current[key], partial[key]),
    );
    if (!differs) return;
    this.next = { ...current, ...partial };
    for (const key of Object.keys(partial) as (keyof S)[]) this.copied.delete(key);
  }

  commit(): boolean {
    if (this.next === null) return false;
    const next = this.next;
    this.next = null;
    this.copied.clear();
    this.store.setState(next, true);
    return true;
  }

  private mapForWrite<K extends MapKeys<S>>(field: K): Record<string, ValueOf<S, K>> {
    this.next ??= { ...this.store.getState() };
    if (!this.copied.has(field)) {
      this.next[field] = { ...(this.next[field] as object) } as S[K];
      this.copied.add(field);
    }
    return this.next[field] as Record<string, ValueOf<S, K>>;
  }
}

/** Structural equality of JSON-like values (DTOs). */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((item, i) => deepEqual(item, b[i]));
  }
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  const rb = b as Record<string, unknown>;
  const ra = a as Record<string, unknown>;
  return ka.every((key) => Object.hasOwn(rb, key) && deepEqual(ra[key], rb[key]));
}

/**
 * A snapshot list as an id map that reuses the previous objects that did not change, so a
 * refetch re-renders only what moved.
 */
export function reconcile<V>(
  previous: ById<V>,
  list: readonly V[],
  keyOf: (value: V) => string,
): ById<V> {
  const next: Record<string, V> = {};
  let same = Object.keys(previous).length === list.length;
  for (const value of list) {
    const id = keyOf(value);
    const old = Object.hasOwn(previous, id) ? previous[id] : undefined;
    if (old !== undefined && deepEqual(old, value)) {
      next[id] = old;
    } else {
      next[id] = value;
      same = false;
    }
  }
  return same ? previous : next;
}
