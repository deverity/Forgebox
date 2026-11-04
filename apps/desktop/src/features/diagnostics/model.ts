// Pure model of the Diagnostics screen (docs/07 §5.12): grouping of checks by id prefix, the
// client fix allowlist, merging of a re-run into the list, and the support bundle that `Exportar
// relatório` writes. The bundle type mirrors `SupportBundle` of `crates/forge-cli/src/commands/
// daemon.rs` (`forge doctor --export`); `support-bundle.golden.json` is checked by a vitest here
// and a cargo test there, so the two cannot drift.
import { FIX_ALLOWLIST } from "../../actions/diagnostics";
import type * as T from "../../ipc/generated";

/** Group of a check: the first segment of its id (`dns.policy.test` -> `dns`, `port.80` -> `port`). */
export function groupOf(id: string): string {
  const dot = id.indexOf(".");
  return dot === -1 ? id : id.slice(0, dot);
}

/** Display order of the groups; unknown groups follow alphabetically. */
export const GROUP_ORDER: readonly string[] = [
  "root",
  "store",
  "dns",
  "tls",
  "port",
  "runtime",
  "path",
  "defender",
  "versions",
  "registry",
  "service",
];

export interface CheckGroup {
  group: string;
  checks: T.CheckDto[];
  /** The worst status in the group. */
  worst: T.CheckStatus;
}

const SEVERITY: Record<T.CheckStatus, number> = { ok: 0, skipped: 1, warn: 2, fail: 3 };

export function worstOf(checks: readonly T.CheckDto[]): T.CheckStatus {
  return checks.reduce<T.CheckStatus>(
    (worst, check) => (SEVERITY[check.status] > SEVERITY[worst] ? check.status : worst),
    "ok",
  );
}

export function groupChecks(checks: readonly T.CheckDto[]): CheckGroup[] {
  const byGroup = new Map<string, T.CheckDto[]>();
  for (const check of checks) {
    const group = groupOf(check.id);
    byGroup.set(group, [...(byGroup.get(group) ?? []), check]);
  }
  const rank = (group: string) => {
    const index = GROUP_ORDER.indexOf(group);
    return index === -1 ? GROUP_ORDER.length : index;
  };
  return [...byGroup.entries()]
    .sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
    .map(([group, list]) => ({ group, checks: list, worst: worstOf(list) }));
}

/** Whether the client may run `check.fix_method` itself (otherwise only "Copiar comando"). */
export function canFix(check: T.CheckDto): boolean {
  return (
    check.status !== "ok" &&
    typeof check.fix_method === "string" &&
    Object.hasOwn(FIX_ALLOWLIST, check.fix_method)
  );
}

/** The text "Copiar comando" copies: the suggested `fix`, if there is one. */
export function copyableFix(check: T.CheckDto): string | null {
  return check.fix !== null && check.fix !== "" ? check.fix : null;
}

/**
 * Replaces the rows of the re-run groups with the fresh ones (`doctor {checks: ['dns']}` returns
 * every `dns.*` row; one that disappeared is gone). Rows keep their group's position.
 */
export function mergeRerun(
  current: readonly T.CheckDto[],
  fresh: readonly T.CheckDto[],
  group: string,
): T.CheckDto[] {
  const kept = current.filter((check) => groupOf(check.id) !== group);
  const firstAt = current.findIndex((check) => groupOf(check.id) === group);
  const at = firstAt === -1 ? kept.length : firstAt;
  return [...kept.slice(0, at), ...fresh, ...kept.slice(at)];
}

export function totals(checks: readonly T.CheckDto[]): Record<T.CheckStatus, number> {
  const out: Record<T.CheckStatus, number> = { ok: 0, warn: 0, fail: 0, skipped: 0 };
  for (const check of checks) out[check.status] += 1;
  return out;
}

/** `port.80` -> 80; other ids -> null. */
export function portOf(check: T.CheckDto): number | null {
  const match = /^port\.(\d{1,5})$/.exec(check.id);
  return match?.[1] === undefined ? null : Number(match[1]);
}

// --- support bundle ------------------------------------------------------------------------

/** Daemon log lines kept in a bundle (the CLI's `EXPORT_LOG_LINES`). */
export const EXPORT_LOG_LINES = 500;

/** Shape of `forge doctor --export`; field names and order are the CLI's. */
export interface SupportBundle {
  v: 1;
  created_unix: number;
  forge_version: string;
  api_version: string;
  doctor: T.DoctorReportDto;
  status: T.DaemonStatusDto;
  /** Installed tool versions as `[id, versions]` pairs. */
  tools: [string, string[]][];
  /** Newest daemon log lines, oldest first. */
  daemon_log: string[];
}

export interface BundleInput {
  checks: readonly T.CheckDto[];
  status: T.DaemonStatusDto;
  tools: readonly T.ToolDto[];
  /** `null` when the log could not be read: one line says so and the rest is exported. */
  log: readonly T.LogLineDto[] | null;
  logError?: string;
  /** Version of the build that produced the report (`hello.daemon_version`). */
  version: string;
  apiVersion: T.ApiVersion;
  /** Seconds since the epoch. */
  now: number;
}

export function buildBundle(input: BundleInput): SupportBundle {
  return {
    v: 1,
    created_unix: input.now,
    forge_version: input.version,
    api_version: `${String(input.apiVersion.major)}.${String(input.apiVersion.minor)}`,
    doctor: { checks: [...input.checks] },
    status: input.status,
    tools: input.tools.map((tool) => [tool.id, tool.installed.map((v) => v.version)]),
    daemon_log:
      input.log === null
        ? [`(daemon log unavailable: ${input.logError ?? "unknown error"})`]
        : input.log.map((line) => line.text),
  };
}

/** The file content: pretty JSON like the CLI's `to_string_pretty` (2 spaces). */
export function bundleJson(bundle: SupportBundle): string {
  return JSON.stringify(bundle, null, 2);
}

/** `forgebox-diagnostics-20261104-153000.json` from a unix time (UTC, sortable). */
export function bundleFileName(unix: number): string {
  const iso = new Date(unix * 1000).toISOString();
  return `forgebox-diagnostics-${iso.slice(0, 10).replaceAll("-", "")}-${iso.slice(11, 19).replaceAll(":", "")}.json`;
}
