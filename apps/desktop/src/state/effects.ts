// What a finished mutation makes stale (docs/03 §7.5, ADR-0034 addendum item 7). The daemon
// publishes `job.*` for every job but an entity event only for some changes: nothing for
// `services.create` without autostart, `services.remove`, `tools.remove`, `services.set`,
// pools stopped by a removal... So when a job ends (`job.done|failed|cancelled`, from this
// GUI, the CLI or MCP alike) the reducer looks its method up here — `JobDto.name` up to the
// first space, the daemon names every job `<method> <args…>` — and refetches the listed
// snapshots (`tx.refresh`, debounced with everything else) and invalidates the listed
// queries. A mutating *call* sent through the reconnect queue applies its row when it
// resolves. Rows are per method, never per tool: the snapshots say what actually changed.
//
// The type demands a row for every job method and every mutating call (possibly empty), so
// a new method forces a decision here; `effects.test.tsx` checks it at runtime too. A failed
// or cancelled job applies its row as well: a multi-step job may stop half-way.
import type { METHODS, MethodName } from "../ipc/methods";
import { QUERY_GROUPS as Q } from "./queries";
import type { SliceName } from "./stores";

type Meta = typeof METHODS;

/** Every job method and every mutating call. */
export type EffectMethod = {
  [M in MethodName]: Meta[M]["kind"] extends "job"
    ? M
    : Meta[M]["mutating"] extends true
      ? M
      : never;
}[MethodName];

export interface MutationEffect {
  /** Snapshots refetched (debounced) when the mutation ends. */
  readonly refresh: readonly SliceName[];
  /** Query methods whose cached answers it makes stale. */
  readonly invalidate: readonly MethodName[];
}

const NONE: MutationEffect = { refresh: [], invalidate: [] };

function effect(
  refresh: readonly SliceName[],
  ...groups: (readonly MethodName[])[]
): MutationEffect {
  return { refresh, invalidate: [...new Set(groups.flat())] };
}

/** Lifecycle jobs: `service.*` events carry the state; ports, pids and pools come back here. */
const LIFECYCLE = effect(["services"], Q.services, Q.pools);
const TOOL_VERSIONS = effect(["tools", "services"], Q.tools, Q.services, Q.pools);
const SITE = effect(["sites"], Q.sites);
const SITE_AND_POOLS = effect(["sites", "services"], Q.sites, Q.pools);
const PHP_RUNTIME = effect(["services"], Q.pools, ["php.ext_list", "php.ini", "sites.info"]);
const CASE = effect(["sites"], ["fs_parity.case_status", "sites.info"]);
const LAN = effect(["sites"], Q.sites, ["net.lan_qr", "services.ports"]);
const REGISTRY = effect(["tools"], Q.tools, ["registry.list"]);
const REMOTE = effect([], ["remote.clients"]);
const EVERYTHING = effect(["tools", "services", "sites"], Q.tools, Q.services, Q.pools, Q.sites);

export const JOB_EFFECTS: Readonly<Record<EffectMethod, MutationEffect>> = {
  // The daemon going down/up is followed by the connection state and `daemon.ready`.
  "daemon.stop": NONE,
  "daemon.restart": NONE,
  "daemon.doctor": NONE,

  "tools.install": TOOL_VERSIONS,
  "tools.remove": TOOL_VERSIONS,
  "tools.update": TOOL_VERSIONS,
  "tools.check_updates": effect(["tools"], ["tools.list", "tools.info"]),
  "tools.rollback": TOOL_VERSIONS,
  "tools.policy": effect(["tools"], ["tools.list", "tools.info"]),
  "tools.add_local": TOOL_VERSIONS,
  "tools.discover": TOOL_VERSIONS,
  "tools.use": TOOL_VERSIONS,

  "services.start": LIFECYCLE,
  "services.stop": LIFECYCLE,
  "services.restart": LIFECYCLE,
  "services.reload": LIFECYCLE,
  "services.create": effect(["services"], Q.services, Q.config),
  "services.remove": effect(["services"], Q.services, Q.config),
  "services.set": effect(["services"], Q.services, Q.config),

  "php.ext_enable": PHP_RUNTIME,
  "php.ext_disable": PHP_RUNTIME,
  "php.xdebug": PHP_RUNTIME,

  "sites.add": SITE_AND_POOLS,
  "sites.remove": SITE_AND_POOLS,
  "sites.php": SITE_AND_POOLS,
  "sites.secure": effect(["sites"], Q.sites, Q.tls),
  "sites.server": effect(["sites", "services"], Q.sites, Q.services),
  "sites.alias": SITE,
  "sites.park": SITE,
  "sites.unpark": SITE,
  "sites.proxy_add": SITE,
  "sites.up": effect(["sites", "services"], Q.sites, Q.services),
  "sites.down": effect(["sites", "services"], Q.sites, Q.services),
  "sites.run": SITE,
  "sites.share": SITE,
  "sites.new": effect(["sites", "services"], Q.sites, Q.pools, ["db.list"]),

  "db.create": effect([], ["db.list"]),
  "db.drop": effect([], ["db.list"]),
  "db.export": NONE,
  "db.import": effect([], ["db.list"]),

  "net.dns_enable": effect(["dns"], Q.dns),
  "net.dns_disable": effect(["dns"], Q.dns),
  "net.lan_expose": LAN,
  "net.lan_unexpose": LAN,

  "tls.ca_install": effect(["ca"], Q.tls),
  "tls.ca_rotate": effect(["ca", "sites"], Q.tls),
  "tls.issue": effect([], Q.tls),
  "tls.renew": effect([], Q.tls),

  "mail.clear": effect([], ["mail.list", "mail.show", "mail.status"]),

  "shell.path_add": effect([], ["shell.path_show"]),
  "shell.path_remove": effect([], ["shell.path_show"]),

  "fs_parity.case_enable": CASE,
  "fs_parity.case_disable": CASE,
  "fs_parity.case_convert": CASE,
  "fs_parity.site_case": CASE,
  "fs_parity.lint_case": effect([], ["fs.read", "fs.list"]),

  // `job.cancelled` already ends the job in the store.
  "jobs.cancel": effect([], Q.jobs),

  "stats.export": NONE,

  "remote.pair": REMOTE,
  "remote.listen": REMOTE,
  "remote.revoke": REMOTE,

  "registry.add": REGISTRY,
  "registry.remove": REGISTRY,
  "registry.update": REGISTRY,

  "system.import_laragon": EVERYTHING,
  "system.bundle_export": NONE,
  "system.bundle_import": EVERYTHING,
  "system.backup_now": NONE,
  "system.backup_schedule": effect([], ["system.config_get"]),
  "system.config_set": effect(["daemon"], ["system.config_get", "daemon.status"]),
  // The daemon restarts after an update: `daemon.ready` resyncs everything.
  "system.self_update": NONE,

  "fs.write": effect([], ["fs.read", "fs.list", "fs.tail"], Q.config),
};

/** Whether `method` has a `JOB_EFFECTS` row (a job or a mutating call of this API). */
export function isEffectMethod(method: string): method is EffectMethod {
  return Object.hasOwn(JOB_EFFECTS, method);
}

/**
 * The method of a job from its `JobDto.name` (`services.remove mysql/default` →
 * `services.remove`); `null` for a name this table does not know (a newer daemon).
 */
export function jobMethod(name: string): EffectMethod | null {
  const space = name.indexOf(" ");
  const method = space < 0 ? name : name.slice(0, space);
  return isEffectMethod(method) ? method : null;
}

/** The effects a sink applies: snapshot refetches and query invalidations. */
export interface EffectSink {
  refresh(slice: SliceName): void;
  invalidate(methods: readonly MethodName[]): void;
}

/** Applies the row of `method`, if any. Returns whether it had one. */
export function applyMutationEffect(sink: EffectSink, method: string): boolean {
  if (!isEffectMethod(method)) return false;
  const row = JOB_EFFECTS[method];
  for (const slice of row.refresh) sink.refresh(slice);
  sink.invalidate(row.invalidate);
  return true;
}
