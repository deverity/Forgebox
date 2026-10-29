// TanStack Query over the API: one key per (context, method, params), and the rules that
// invalidate those keys when events arrive (debounced, so a burst of 1,000 `site.*` events
// refetches `sites.list` once). Queries serve screen data that is not kept in the entity
// stores (details, catalog, logs...); the stores serve the live lists.
import {
  queryOptions,
  useQuery,
  type QueryClient,
  type UseQueryResult,
} from "@tanstack/react-query";
import type { CallMethod, ForgeApi, ParamsOf, ResultOf } from "../ipc/api";
import type { ForgeApiError } from "../ipc/errors";
import type { EventKind, MethodName } from "../ipc/methods";

/** `[context, method, params]`; `[context, method]` matches every params of a method. */
export type ForgeQueryKey = readonly [context: string, method: string, params: unknown];

export const queryKeys = {
  context: (context: string) => [context] as const,
  method: (context: string, method: MethodName) => [context, method] as const,
  call: <M extends CallMethod>(context: string, method: M, params: ParamsOf<M>) =>
    [context, method, params] as const,
};

/** Query options of one call (`useQuery(forgeQuery(api, "sites.info", {site}))`). */
export function forgeQuery<M extends CallMethod>(api: ForgeApi, method: M, params: ParamsOf<M>) {
  return queryOptions<ResultOf<M>, ForgeApiError, ResultOf<M>, ForgeQueryKey>({
    queryKey: queryKeys.call(api.context, method, params),
    queryFn: () => api.call(method, params),
  });
}

export function useForgeQuery<M extends CallMethod>(
  api: ForgeApi,
  method: M,
  params: ParamsOf<M>,
  options: { enabled?: boolean } = {},
): UseQueryResult<ResultOf<M>, ForgeApiError> {
  return useQuery({ ...forgeQuery(api, method, params), enabled: options.enabled ?? true });
}

const SITES: readonly MethodName[] = ["sites.list", "sites.info", "sites.parked", "sites.ps"];
const TOOLS: readonly MethodName[] = [
  "tools.list",
  "tools.info",
  "tools.search",
  "tools.which",
  "tools.history",
  "php.list",
  "php.ext_list",
];
const SERVICES: readonly MethodName[] = [
  "services.status",
  "services.ports",
  "services.config_files",
  "sites.ps",
  "mail.status",
];
const POOLS: readonly MethodName[] = ["php.pool_status", "php.list", "services.status"];
const JOBS: readonly MethodName[] = ["jobs.list", "jobs.get"];
const CONFIG: readonly MethodName[] = [
  "services.config",
  "services.config_files",
  "services.settings",
  "php.ini",
  "fs.read",
];
const DNS: readonly MethodName[] = ["net.dns_status", "net.dns_test"];
const TLS: readonly MethodName[] = ["tls.ca_status", "tls.list", "sites.info", "sites.list"];

/** The query groups the event rows use, shared with `JOB_EFFECTS` (effects.ts). */
export const QUERY_GROUPS = {
  sites: SITES,
  tools: TOOLS,
  services: SERVICES,
  pools: POOLS,
  jobs: JOBS,
  config: CONFIG,
  dns: DNS,
  tls: TLS,
} as const;

/**
 * Methods whose cached answers an event makes stale. Every kind has a row (possibly empty),
 * so a new kind forces a decision here too.
 */
export const EVENT_INVALIDATIONS: Readonly<Record<EventKind, readonly MethodName[]>> = {
  "daemon.ready": ["daemon.status", "system.hello"],
  "daemon.shutdown": [],
  "job.started": JOBS,
  "job.progress": [],
  "job.done": JOBS,
  "job.failed": JOBS,
  "job.cancelled": JOBS,
  "actor.restarted": [],
  "actor.failed": ["daemon.status"],
  "fs.case_sensitivity_applied": ["fs_parity.case_status", "sites.info"],
  "fs.case_sensitivity_unavailable": ["fs_parity.case_status", "sites.info"],
  "tool.install.started": [],
  "tool.install.progress": [],
  "tool.install.completed": TOOLS,
  "tool.install.failed": TOOLS,
  "tool.updates_available": ["tools.list", "tools.info"],
  "tool.updated": [...TOOLS, "services.status"],
  "tool.rollback": [...TOOLS, "services.status"],
  "tool.current_changed": ["tools.list", "tools.info", "tools.which", "php.list"],
  "service.starting": SERVICES,
  "service.running": SERVICES,
  "service.stopping": SERVICES,
  "service.stopped": SERVICES,
  "service.crashed": SERVICES,
  "service.failed": SERVICES,
  "service.maintenance": SERVICES,
  "service.metrics": [],
  "pool.scaled": POOLS,
  "pool.worker.recycled": ["php.pool_status"],
  "site.created": SITES,
  "site.updated": SITES,
  "site.removed": SITES,
  "site.php_changed": [...SITES, ...POOLS],
  "config.rendered": CONFIG,
  "config.drift_detected": CONFIG,
  "config.changed": ["system.config_get", "system.config_list", "daemon.status"],
  "dns.policy_applied": DNS,
  "tls.ca_created": TLS,
  "tls.leaf_issued": TLS,
  "tls.renewed": TLS,
  "tls.ca_untrusted": TLS,
  "port.conflict": ["services.ports", "services.status"],
  "port.holder_killed": ["services.ports", "services.status", "net.port_check"],
  "helper.op": [...DNS, "tls.ca_status", "shell.path_show"],
  "mail.changed": ["mail.status", "mail.list"],
  "terminal.exited": ["terminal.list"],
};

/**
 * Collects the methods to invalidate and invalidates each once per `debounceMs` window
 * (`[context, method]` prefix: every params of the method).
 */
export class Invalidator {
  private readonly pending = new Set<string>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly client: QueryClient;
  private readonly context: string;
  private readonly debounceMs: number;

  constructor(client: QueryClient, context: string, debounceMs = 200) {
    this.client = client;
    this.context = context;
    this.debounceMs = debounceMs;
  }

  /** Marks the methods an event of `kind` makes stale. */
  onEvent(kind: string): void {
    const methods = (EVENT_INVALIDATIONS as Record<string, readonly string[] | undefined>)[kind];
    if (methods !== undefined) this.invalidate(methods);
  }

  /** Marks `methods` stale (a finished mutation, `JOB_EFFECTS`); same debounce window. */
  invalidate(methods: readonly string[]): void {
    if (methods.length === 0) return;
    for (const method of methods) this.pending.add(method);
    this.timer ??= setTimeout(() => {
      this.flush();
    }, this.debounceMs);
  }

  /** Every query of the context (after a resync). */
  all(): void {
    this.pending.clear();
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    void this.client.invalidateQueries({ queryKey: queryKeys.context(this.context) });
  }

  flush(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    const methods = [...this.pending];
    this.pending.clear();
    for (const method of methods) {
      void this.client.invalidateQueries({ queryKey: [this.context, method] });
    }
  }

  dispose(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.pending.clear();
  }
}
