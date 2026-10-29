// The event reducers (docs/03 §8): every `EVENT_KINDS` entry is either reduced into the
// stores or explicitly ignored with a reason — the registry's type demands an entry per kind
// and `reducers.test.ts` checks it at runtime too. Reducers are projections: they copy what
// the payload says into the stores, or ask for a snapshot refetch when the payload is not
// enough to build the DTO (`tx.refresh`). Kinds a newer daemon adds are ignored. A job that
// ends also applies the `JOB_EFFECTS` row of its method (effects.ts): many mutations publish
// no entity event, so the snapshots they touch are refetched and their queries invalidated.
import { failureCode } from "../ipc/describeError";
import type * as T from "../ipc/generated";
import { EVENT_KINDS, type EventKind, type EventPayloads, type MethodName } from "../ipc/methods";
import { applyMutationEffect, jobMethod } from "./effects";
import {
  Draft,
  type AttentionSlice,
  type ConnectionSlice,
  type ContextStores,
  type JobsSlice,
  type ServicesSlice,
  type SitesSlice,
  type SliceName,
  type SystemSlice,
  type ToolInstallEntry,
  type ToolsSlice,
} from "./stores";

/** The working copies of one batch plus the effects reducers may ask for. */
export interface Tx {
  readonly connection: Draft<ConnectionSlice>;
  readonly services: Draft<ServicesSlice>;
  readonly sites: Draft<SitesSlice>;
  readonly tools: Draft<ToolsSlice>;
  readonly jobs: Draft<JobsSlice>;
  readonly system: Draft<SystemSlice>;
  readonly attention: Draft<AttentionSlice>;
  /** Refetch one snapshot (debounced) when the payload cannot build the entity. */
  refresh(slice: SliceName): void;
  /** Invalidate the cached queries of `methods` (debounced; `JOB_EFFECTS`). */
  invalidate(methods: readonly MethodName[]): void;
  /** Refetch every snapshot (debounced). */
  resync(reason: string): void;
}

/** What a batch may ask of the runtime besides store writes. */
export interface TxEffects {
  refresh(slice: SliceName): void;
  invalidate(methods: readonly MethodName[]): void;
  resync(reason: string): void;
}

export type Reducer<K extends EventKind> = (
  tx: Tx,
  payload: EventPayloads[K],
  event: T.EventDto,
) => void;

/** A kind the stores do not use, and why. */
export interface Ignored {
  readonly ignore: string;
}

export type ReducerEntry<K extends EventKind> = Reducer<K> | Ignored;
export type ReducerRegistry = { readonly [K in EventKind]: ReducerEntry<K> };

/** Terminal jobs kept in the store (the daemon keeps the last 100). */
export const MAX_TERMINAL_JOBS = 200;
/** Failed tool installs kept in the store. */
const MAX_INSTALL_ENTRIES = 50;

const SERVICE_STATE: Record<string, T.ServiceState> = {
  "service.starting": "starting",
  "service.running": "running",
  "service.stopping": "stopping",
  "service.stopped": "stopped",
  "service.crashed": "crashed",
  "service.failed": "failed",
  "service.maintenance": "maintenance",
};

/**
 * `InstanceDto.last_error` from a `service.failed|crashed` payload, with the diagnosis in
 * `details` the way the daemon writes it (`kind` + fields, plus `{reason, args}`, ADR-0039).
 */
export function serviceError(payload: T.ServicePayload): T.ApiError | null {
  const reason = payload.reason ?? null;
  const details =
    reason === null
      ? (payload.error?.details ?? null)
      : (() => {
          const { kind, ...args } = reason;
          return { ...reason, reason: kind, args };
        })();
  if (payload.error !== null) {
    return { code: payload.error.code, message: payload.error.message, details };
  }
  if (reason === null) return null;
  return {
    code: failureCode(reason),
    message: reason.kind === "other" ? reason.message : reason.kind,
    details,
  };
}

function reduceService(tx: Tx, payload: T.ServicePayload, event: T.EventDto): void {
  const state = SERVICE_STATE[event.kind] ?? payload.state;
  const id = payload.instance_id;
  const current = tx.services.get("instances", id);
  if (current === undefined) {
    // A new instance (services.create, an import): its DTO comes from services.status.
    tx.refresh("services");
  } else {
    const next: T.InstanceDto = { ...current, state, pid: payload.pid };
    if (state === "running") {
      if (current.state !== "running") next.started_at = event.ts;
      next.last_error = null;
    } else if (state === "stopped") {
      next.pid = null;
      next.started_at = null;
      next.cpu_pct = null;
      next.rss_bytes = null;
    } else if (state === "failed" || state === "crashed") {
      const error = serviceError(payload);
      if (error !== null) next.last_error = error;
    }
    tx.services.put("instances", id, next);
  }
  if (state === "failed" || state === "crashed") {
    tx.attention.put("serviceFailures", id, { ...payload, state, ts: event.ts });
  } else if (state === "running" || state === "stopped") {
    tx.attention.remove("serviceFailures", id);
  }
  if (state === "running") {
    tx.attention.removeWhere("portConflicts", (conflict) => conflict.instance_id === id);
  }
}

function jobBase(jobId: string, name: string, ts: string, requestId: string | null): T.JobDto {
  return {
    id: jobId,
    name,
    request_id: requestId,
    state: "running",
    progress: {},
    cancel_requested: false,
    started_at: ts,
    finished_at: null,
    result: null,
    error: null,
  };
}

function finishJob(
  tx: Tx,
  payload: { job_id: string; name: string },
  event: T.EventDto,
  patch: Partial<T.JobDto>,
): void {
  const stored = tx.jobs.get("jobs", payload.job_id);
  // Applied even when the safety poll saw the end first: each terminal event arrives once
  // (the stream drops seq it already saw), and the poll writes only the jobs store.
  const method = jobMethod(payload.name === "" ? (stored?.name ?? "") : payload.name);
  if (method !== null) applyMutationEffect(tx, method);
  const current = stored ?? jobBase(payload.job_id, payload.name, event.ts, null);
  if (current.state !== "running") return; // Terminal states never change.
  tx.jobs.put("jobs", payload.job_id, { ...current, finished_at: event.ts, ...patch });
}

function installKey(payload: T.ToolInstallPayload): string {
  return payload.job_id ?? `${payload.tool}@${payload.version}`;
}

function reduceInstall(tx: Tx, payload: T.ToolInstallPayload, event: T.EventDto): void {
  const key = installKey(payload);
  const previous = tx.tools.get("installs", key);
  const entry: ToolInstallEntry = {
    key,
    tool: payload.tool,
    version: payload.version,
    jobId: payload.job_id,
    state: event.kind === "tool.install.failed" ? "failed" : "installing",
    step: payload.step ?? previous?.step ?? null,
    progress: payload.progress ?? previous?.progress ?? null,
    error: payload.error,
    ts: event.ts,
  };
  tx.tools.put("installs", key, entry);
}

function refreshTools(tx: Tx): void {
  tx.refresh("tools");
}

export const EVENT_REDUCERS: ReducerRegistry = {
  "daemon.ready": (tx, payload) => {
    tx.connection.patch({ daemonVersion: payload.version, shutdownReason: null });
    // A daemon (re)start: what it runs now comes from the snapshots.
    tx.resync("daemon_ready");
  },
  "daemon.shutdown": (tx, payload) => {
    tx.connection.patch({ shutdownReason: payload.reason });
  },

  "job.started": (tx, payload, event) => {
    if (tx.jobs.get("jobs", payload.job_id) !== undefined) return;
    tx.jobs.put(
      "jobs",
      payload.job_id,
      jobBase(payload.job_id, payload.name, event.ts, payload.request_id ?? null),
    );
  },
  "job.progress": (tx, payload, event) => {
    const current =
      tx.jobs.get("jobs", payload.job_id) ?? jobBase(payload.job_id, "", event.ts, null);
    if (current.state !== "running") return;
    const progress: T.JobProgress = {
      pct: payload.pct ?? null,
      message: payload.message ?? null,
      bytes_done: payload.bytes_done ?? null,
      bytes_total: payload.bytes_total ?? null,
      speed_bps: payload.speed_bps ?? null,
      eta_secs: payload.eta_secs ?? null,
    };
    tx.jobs.put("jobs", payload.job_id, { ...current, progress });
  },
  "job.done": (tx, payload, event) => {
    finishJob(tx, payload, event, { state: "done", result: payload.result });
  },
  "job.failed": (tx, payload, event) => {
    finishJob(tx, payload, event, { state: "failed", error: payload.error });
  },
  "job.cancelled": (tx, payload, event) => {
    finishJob(tx, payload, event, { state: "cancelled", cancel_requested: true });
  },

  "actor.restarted": { ignore: "transient: the actor supervisor already recovered it" },
  "actor.failed": (tx, payload, event) => {
    tx.attention.put("actorFailures", payload.actor, { ...payload, ts: event.ts });
  },

  "fs.case_sensitivity_applied": (tx, payload) => {
    tx.attention.remove("caseUnavailable", payload.dir);
  },
  "fs.case_sensitivity_unavailable": (tx, payload, event) => {
    tx.attention.put("caseUnavailable", payload.dir, { ...payload, ts: event.ts });
  },

  "tool.install.started": reduceInstall,
  "tool.install.progress": reduceInstall,
  "tool.install.completed": (tx, payload) => {
    tx.tools.remove("installs", installKey(payload));
    refreshTools(tx);
  },
  "tool.install.failed": reduceInstall,
  "tool.updates_available": (tx, payload) => {
    // Always the whole pending list: replace (an empty list clears the badge).
    tx.tools.patch({ updates: payload.updates, updateCount: payload.count });
  },
  "tool.updated": refreshTools,
  "tool.rollback": refreshTools,
  "tool.current_changed": (tx, payload) => {
    const tool = tx.tools.get("tools", payload.tool);
    if (tool === undefined) {
      refreshTools(tx);
      return;
    }
    tx.tools.put("tools", payload.tool, {
      ...tool,
      current: payload.to,
      installed: tool.installed.map((version) => ({
        ...version,
        is_current:
          payload.version_id === null
            ? version.version === payload.to
            : version.id === payload.version_id,
      })),
    });
  },

  "service.starting": reduceService,
  "service.running": reduceService,
  "service.stopping": reduceService,
  "service.stopped": reduceService,
  "service.crashed": reduceService,
  "service.failed": reduceService,
  "service.maintenance": reduceService,
  "service.metrics": {
    ignore: "metrics come from the services.status poll (useMetricsPoll, ADR-0034 state layer)",
  },

  "pool.scaled": (tx, payload) => {
    const pool = tx.services.get("pools", payload.version_id);
    if (pool === undefined) {
      tx.refresh("services");
      return;
    }
    tx.services.put("pools", payload.version_id, { ...pool, workers: payload.to_workers });
  },
  "pool.worker.recycled": {
    ignore: "worker churn changes no entity; php.pool_status queries are invalidated",
  },

  "site.created": (tx) => {
    tx.refresh("sites");
  },
  "site.updated": (tx) => {
    tx.refresh("sites");
  },
  "site.removed": (tx, payload) => {
    tx.sites.remove("sites", payload.site_id);
  },
  "site.php_changed": (tx) => {
    // The site's PHP and the pools it uses both moved.
    tx.refresh("sites");
    tx.refresh("services");
  },

  "config.rendered": (tx, payload) => {
    tx.attention.remove("configDrift", payload.path);
  },
  "config.drift_detected": (tx, payload, event) => {
    tx.attention.put("configDrift", payload.path, { ...payload, ts: event.ts });
  },

  "dns.policy_applied": (tx, payload) => {
    tx.system.put("dnsPolicies", payload.tld, payload);
    tx.refresh("dns");
  },
  "tls.ca_created": (tx) => {
    tx.refresh("ca");
  },
  "tls.leaf_issued": (tx, payload, event) => {
    tx.system.put("certificates", payload.host ?? payload.thumbprint_sha256, {
      ...payload,
      ts: event.ts,
    });
  },
  "tls.renewed": (tx, payload, event) => {
    tx.system.put("certificates", payload.host ?? payload.thumbprint_sha256, {
      ...payload,
      ts: event.ts,
    });
  },

  "port.conflict": (tx, payload, event) => {
    tx.attention.put("portConflicts", `${payload.protocol}/${String(payload.port)}`, {
      ...payload,
      ts: event.ts,
    });
  },
  "helper.op": (tx, payload, event) => {
    if (payload.ok) tx.attention.remove("helperFailures", payload.name);
    else tx.attention.put("helperFailures", payload.name, { ...payload, ts: event.ts });
    // Elevated operations change DNS routing and trust stores.
    tx.refresh("dns");
    tx.refresh("ca");
  },
};

export function isIgnored(entry: ReducerEntry<EventKind>): entry is Ignored {
  return typeof entry === "object";
}

const KNOWN = new Set<string>(EVENT_KINDS);

export function isEventKind(kind: string): kind is EventKind {
  return KNOWN.has(kind);
}

/** Opens the drafts of one batch. */
export function openTx(stores: ContextStores, effects: TxEffects): Tx {
  return {
    connection: new Draft(stores.connection),
    services: new Draft(stores.services),
    sites: new Draft(stores.sites),
    tools: new Draft(stores.tools),
    jobs: new Draft(stores.jobs),
    system: new Draft(stores.system),
    attention: new Draft(stores.attention),
    refresh: (slice) => {
      effects.refresh(slice);
    },
    invalidate: (methods) => {
      effects.invalidate(methods);
    },
    resync: (reason) => {
      effects.resync(reason);
    },
  };
}

function pruneJobs(tx: Tx): void {
  const jobs = Object.values(tx.jobs.state.jobs);
  const terminal = jobs.filter((job) => job.state !== "running");
  if (terminal.length <= MAX_TERMINAL_JOBS) return;
  terminal
    .sort((a, b) => (a.finished_at ?? "").localeCompare(b.finished_at ?? ""))
    .slice(0, terminal.length - MAX_TERMINAL_JOBS)
    .forEach((job) => {
      tx.jobs.remove("jobs", job.id);
    });
}

function pruneInstalls(tx: Tx): void {
  const failed = Object.values(tx.tools.state.installs).filter((i) => i.state === "failed");
  if (failed.length <= MAX_INSTALL_ENTRIES) return;
  failed
    .sort((a, b) => a.ts.localeCompare(b.ts))
    .slice(0, failed.length - MAX_INSTALL_ENTRIES)
    .forEach((entry) => {
      tx.tools.remove("installs", entry.key);
    });
}

/** Applies one batch of events in order. Returns how many were reduced (not ignored). */
export function reduceEvents(tx: Tx, events: readonly T.EventDto[]): number {
  let reduced = 0;
  for (const event of events) {
    if (!isEventKind(event.kind)) continue; // A newer daemon's kind.
    const entry = EVENT_REDUCERS[event.kind] as ReducerEntry<EventKind>;
    if (isIgnored(entry)) continue;
    if (typeof event.payload !== "object" || event.payload === null) continue;
    try {
      entry(tx, event.payload as EventPayloads[EventKind], event);
      reduced += 1;
    } catch (error) {
      console.warn(`state: reducer of ${event.kind} failed (seq ${String(event.seq)})`, error);
    }
  }
  pruneJobs(tx);
  pruneInstalls(tx);
  return reduced;
}

/** Commits every draft that changed: at most one `setState` per store. */
export function commitTx(tx: Tx): number {
  let commits = 0;
  for (const draft of [
    tx.connection,
    tx.services,
    tx.sites,
    tx.tools,
    tx.jobs,
    tx.system,
    tx.attention,
  ] as Draft<object>[]) {
    if (draft.commit()) commits += 1;
  }
  return commits;
}
