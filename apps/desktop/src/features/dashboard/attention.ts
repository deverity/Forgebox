// The Atenção sources of the Dashboard (docs/07 section 5.2): pure functions from the data the
// state layer already holds (stores, events, cached queries) to `AttentionItem`s. Nothing here
// asks the daemon for a verdict: there is no `daemon.attention` method (M2 decision); each
// source decides from the facts. The equivalent `forge` command always comes from `renderCli`.
import type { AttentionItem } from "../../app/slots";
import type { I18nText } from "../../ipc/describeError";
import { renderCli } from "../../ipc/cli";
import type { MethodName } from "../../ipc/methods";
import type * as T from "../../ipc/generated";

export const DASHBOARD_NS = "dashboard";

/** Certificates closer than this to `not_after` are flagged (docs/07 section 5.2). */
export const CERT_WARN_DAYS = 30;
/** The local CA is flagged when it expires sooner than this. */
export const CA_WARN_DAYS = 60;
/** A config drift older than this is no longer news. */
export const DRIFT_WINDOW_MS = 24 * 60 * 60 * 1000;

const DAY_MS = 24 * 60 * 60 * 1000;

function text(key: string, values: Record<string, string | number> = {}): I18nText {
  return { key, values };
}

function cli(method: MethodName, params: unknown = {}): { cli?: string } {
  const command = renderCli(method, params);
  return command === null ? {} : { cli: command };
}

/** Whole days from `now` to `date` (negative once past); `null` for an unparsable date. */
export function daysUntil(date: string, now: number): number | null {
  const at = Date.parse(date);
  return Number.isNaN(at) ? null : Math.floor((at - now) / DAY_MS);
}

/**
 * The program that holds a port, as a short name: `mysqld.exe` from `mysqld.exe (C:/xampp/...)`
 * or from a full path. `null` when the daemon could not tell (`?` or empty): the title then says
 * "another program" instead of "by ?" (R-18).
 */
export function holderName(holder: T.PortHolderDto | null): string | null {
  if (holder === null) return null;
  const head = holder.exe.trim().split(" (")[0] ?? "";
  const base = head.split(/[\\/]/).pop()?.trim() ?? "";
  return base === "" || base === "?" ? null : base;
}

// --- ports ---------------------------------------------------------------------------------

type Stamped<P> = P & { ts: string };

/** External port conflicts: `port.conflict` events plus `services.ports` `external`. */
export function portItems(
  conflicts: Readonly<Record<string, Stamped<T.PortConflictPayload>>>,
  external: readonly T.ExternalPortDto[],
): AttentionItem[] {
  const items = new Map<string, AttentionItem>();
  const add = (
    protocol: string,
    port: number,
    holder: T.PortHolderDto | null,
    owner: string | null,
    severity: AttentionItem["severity"],
  ) => {
    const key = `${protocol}/${String(port)}`;
    if (items.has(key)) return;
    items.set(key, {
      id: `port:${key}`,
      severity,
      ns: DASHBOARD_NS,
      title:
        holderName(holder) === null
          ? text("attention.ports.titleUnknown", { port })
          : text("attention.ports.title", { port, holder: holderName(holder) ?? "" }),
      cause:
        owner === null
          ? text("attention.ports.cause")
          : text("attention.ports.causeOwner", { owner }),
      ...cli("services.ports"),
      href: "/services",
      actionLabel: text("attention.ports.action"),
    });
  };
  for (const conflict of Object.values(conflicts)) {
    add(conflict.protocol, conflict.port, conflict.holder, conflict.owner_name, "critical");
  }
  for (const entry of external) add(entry.protocol, entry.port, entry.holder, null, "warning");
  return [...items.values()];
}

// --- updates -------------------------------------------------------------------------------

/**
 * The pending updates: `tool.updates_available` (live, the whole list) plus the tools whose
 * `tools.list` row says a newer version exists (the only source right after the app opens,
 * since that event is not replayed).
 */
export function pendingUpdates(
  updates: readonly T.PendingUpdateDto[],
  tools: readonly T.ToolDto[],
): T.PendingUpdateDto[] {
  const known = new Set(updates.map((update) => update.tool));
  const fromList = tools
    .filter((tool) => tool.update_available !== null && !known.has(tool.id))
    .map((tool) => ({
      tool: tool.id,
      series: "",
      from: tool.current ?? "",
      to: tool.update_available ?? "",
    }));
  return [...updates, ...fromList];
}

/** One item for all pending tool updates. */
export function updateItems(updates: readonly T.PendingUpdateDto[]): AttentionItem[] {
  if (updates.length === 0) return [];
  return [
    {
      id: "updates",
      severity: "info",
      ns: DASHBOARD_NS,
      title: text("attention.updates.title", { count: updates.length }),
      cause: text("attention.updates.cause"),
      ...cli("tools.check_updates"),
      href: "/catalog",
      actionLabel: text("attention.updates.action"),
    },
  ];
}

// --- config drift --------------------------------------------------------------------------

export interface DriftEvent {
  kind: string;
  ts: string;
  payload: unknown;
}

function pathOf(payload: unknown): string | null {
  if (typeof payload !== "object" || payload === null) return null;
  const path = (payload as { path?: unknown }).path;
  return typeof path === "string" ? path : null;
}

/**
 * Files whose last event (oldest to newest) is a drift newer than 24 h: `config.rendered`
 * writes the file again and clears it. `live` are the drifts the stores saw this session.
 */
export function driftedFiles(
  history: readonly DriftEvent[],
  live: Readonly<Record<string, { ts: string }>>,
  now: number,
): { path: string; ts: string }[] {
  const drift = new Map<string, string>();
  for (const event of history) {
    const path = pathOf(event.payload);
    if (path === null) continue;
    if (event.kind === "config.drift_detected") drift.set(path, event.ts);
    else if (event.kind === "config.rendered") drift.delete(path);
  }
  for (const [path, entry] of Object.entries(live)) drift.set(path, entry.ts);
  return [...drift.entries()]
    .filter(([, ts]) => {
      const at = Date.parse(ts);
      return !Number.isNaN(at) && now - at <= DRIFT_WINDOW_MS;
    })
    .map(([path, ts]) => ({ path, ts }))
    .sort((a, b) => a.path.localeCompare(b.path));
}

export function driftItems(files: readonly { path: string }[]): AttentionItem[] {
  return files.map((file) => ({
    id: `drift:${file.path}`,
    severity: "warning",
    ns: DASHBOARD_NS,
    title: text("attention.drift.title", { path: file.path }),
    cause: text("attention.drift.cause"),
    ...cli("daemon.doctor"),
    href: "/diagnostics",
    actionLabel: text("attention.drift.action"),
  }));
}

// --- certificates --------------------------------------------------------------------------

/** The local CA: not trusted by this user or machine, or expiring within 60 days. */
export function caItems(ca: T.CaStatusDto | null, now: number): AttentionItem[] {
  if (!ca?.exists) return [];
  if (!ca.trusted_current_user && !ca.trusted_local_machine) {
    return [
      {
        id: "ca:untrusted",
        severity: "critical",
        ns: DASHBOARD_NS,
        title: text("attention.caUntrusted.title"),
        cause: text("attention.caUntrusted.cause"),
        ...cli("tls.ca_install"),
        href: "/settings",
        actionLabel: text("attention.caUntrusted.action"),
      },
    ];
  }
  const days = ca.not_after === null ? null : daysUntil(ca.not_after, now);
  if (days !== null && days < CA_WARN_DAYS) {
    return [
      {
        id: "ca:expiring",
        severity: "warning",
        ns: DASHBOARD_NS,
        title: text("attention.caExpiring.title", { days: Math.max(0, days) }),
        cause: text("attention.caExpiring.cause"),
        ...cli("tls.ca_rotate"),
        href: "/settings",
        actionLabel: text("attention.caExpiring.action"),
      },
    ];
  }
  return [];
}

/** Site certificates expiring in less than 30 days (expired ones included): one item. */
export function certItems(certificates: readonly T.CertificateDto[], now: number): AttentionItem[] {
  const expiring = certificates
    .map((cert) => ({ cert, days: daysUntil(cert.not_after, now) }))
    .filter((entry): entry is { cert: T.CertificateDto; days: number } => {
      return entry.days !== null && entry.days < CERT_WARN_DAYS;
    })
    .sort((a, b) => a.days - b.days);
  const first = expiring[0];
  if (first === undefined) return [];
  const expired = first.days < 0;
  return [
    {
      id: "certs",
      severity: expired ? "critical" : "warning",
      ns: DASHBOARD_NS,
      title: text("attention.certs.title", { count: expiring.length }),
      cause: expired
        ? text("attention.certs.causeExpired", { host: first.cert.host })
        : text("attention.certs.cause", { host: first.cert.host, days: first.days }),
      ...cli("tls.renew", { all: true }),
      href: "/settings",
      actionLabel: text("attention.certs.action"),
    },
  ];
}

// --- DNS -----------------------------------------------------------------------------------

const KNOWN_DNS_CAUSES: ReadonlySet<string> = new Set([
  "group_policy_table_present",
  "resolver_probe_failed",
  "resolver_not_on_port53",
  "nrpt_unsupported",
  "none",
  "hosts",
]);

/** The effective routing mode; daemons without `mode` (API 1.0) are read from `policy`. */
function dnsMode(dns: T.DnsStatusDto): T.DnsMode {
  if (dns.mode !== undefined) return dns.mode;
  if (dns.policy === "applied") return "nrpt";
  return dns.policy === "hosts_fallback" ? "hosts" : "none";
}

/** DNS the user turned on that Windows does not route through NRPT, with the reason. */
export function dnsItems(dns: T.DnsStatusDto | null): AttentionItem[] {
  if (dns?.enabled !== true) return [];
  const mode = dnsMode(dns);
  if (mode === "nrpt") return [];
  const reason: string = dns.reason ?? mode;
  const cause = KNOWN_DNS_CAUSES.has(reason) ? reason : mode;
  return [
    {
      id: "dns",
      severity: mode === "hosts" ? "info" : "warning",
      ns: DASHBOARD_NS,
      title: text(mode === "hosts" ? "attention.dns.titleHosts" : "attention.dns.title"),
      cause: text(`attention.dns.cause.${cause}`),
      ...cli("net.dns_status"),
      href: "/diagnostics",
      actionLabel: text("attention.dns.action"),
    },
  ];
}

// --- failed services -----------------------------------------------------------------------

export function failedItems(instances: readonly T.InstanceDto[]): AttentionItem[] {
  return instances
    .filter((instance) => instance.state === "failed" || instance.state === "crashed")
    .map((instance) => {
      const target = `${instance.tool}/${instance.name}`;
      return {
        id: `failed:${instance.id}`,
        severity: "critical",
        ns: DASHBOARD_NS,
        title: text("attention.failed.title", { name: target }),
        cause: text("attention.failed.cause"),
        ...cli("services.restart", { targets: [target] }),
        href: "/services",
        search: { instance: instance.id },
        actionLabel: text("attention.failed.action"),
      } satisfies AttentionItem;
    });
}
