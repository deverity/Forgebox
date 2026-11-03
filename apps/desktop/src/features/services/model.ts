// Pure view-model of the Services screen (docs/07 section 5.3): rows, grouping by the
// category the daemon reports in `tools.list`, state mapping and small formatters. No React and
// no tool names: everything comes from the DTOs.
import type { InstanceDto, PoolDto, ServiceState, ToolDto } from "../../ipc/generated";
import type { ServiceState as DotState } from "../../design/primitives/StatusDot";

/** Category shown when a tool has none. */
export const OTHER_CATEGORY = "other";

/** Display order of the known categories; unknown ones follow alphabetically. */
export const CATEGORY_ORDER = [
  "webserver",
  "database",
  "cache",
  "mail",
  "runtime",
  "devtool",
] as const;

export const PHP_TOOL = "php";

export type ServiceRow =
  | { kind: "instance"; key: string; tool: string; instance: InstanceDto }
  | { kind: "pool"; key: string; tool: string; pool: PoolDto };

export interface ServiceGroup {
  category: string;
  rows: ServiceRow[];
}

/** `mysql/default`: what the CLI and `services.*` take as a target. */
export function targetOf(instance: Pick<InstanceDto, "tool" | "name">): string {
  return `${instance.tool}/${instance.name}`;
}

/** `php@8.3`: the label and log target of a pool. */
export function poolName(pool: Pick<PoolDto, "series">): string {
  return `${PHP_TOOL}@${pool.series}`;
}

/** The dot of the shared StatusDot for a daemon state. */
export function dotState(state: ServiceState): DotState {
  switch (state) {
    case "running":
      return "running";
    case "starting":
    case "stopping":
      return "starting";
    case "failed":
    case "crashed":
      return "failed";
    case "maintenance":
      return "updating";
    case "stopped":
      return "stopped";
  }
}

/** A state from which "Stop" makes sense. */
export function isActive(state: ServiceState): boolean {
  return state === "running" || state === "starting";
}

/** The failure states that carry a diagnosis. */
export function isFailure(state: ServiceState): boolean {
  return state === "failed" || state === "crashed";
}

function rank(category: string): number {
  const index = (CATEGORY_ORDER as readonly string[]).indexOf(category);
  if (index >= 0) return index;
  return category === OTHER_CATEGORY ? CATEGORY_ORDER.length + 1 : CATEGORY_ORDER.length;
}

/** Instances and pools grouped by tool category, in a stable order. */
export function groupRows(
  instances: readonly InstanceDto[],
  pools: readonly PoolDto[],
  tools: Readonly<Record<string, ToolDto>>,
): ServiceGroup[] {
  const rows: ServiceRow[] = [
    ...instances.map((instance): ServiceRow => ({
      kind: "instance",
      key: instance.id,
      tool: instance.tool,
      instance,
    })),
    ...pools.map((pool): ServiceRow => ({
      kind: "pool",
      key: pool.version_id,
      tool: PHP_TOOL,
      pool,
    })),
  ];
  const byCategory = new Map<string, ServiceRow[]>();
  for (const row of rows) {
    const category = tools[row.tool]?.category ?? OTHER_CATEGORY;
    const list = byCategory.get(category) ?? [];
    list.push(row);
    byCategory.set(category, list);
  }
  const label = (row: ServiceRow): string =>
    row.kind === "instance" ? `${row.tool}/${row.instance.name}` : poolName(row.pool);
  return [...byCategory.entries()]
    .sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
    .map(([category, list]) => ({
      category,
      rows: [...list].sort((a, b) => label(a).localeCompare(label(b))),
    }));
}

/** The port named `main`, else the first one. */
export function mainPort(instance: InstanceDto): number | null {
  const ports = instance.ports;
  return (ports.find((port) => port.name === "main") ?? ports[0])?.port ?? null;
}

/** Uptime in ms from `started_at`, or null when it is unknown. */
export function uptimeMs(startedAt: string | null, now: number): number | null {
  if (startedAt === null) return null;
  const started = Date.parse(startedAt);
  return Number.isNaN(started) ? null : Math.max(0, now - started);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** What a `port_in_use` diagnosis says about the process holding the port, when known. */
export function portHolder(
  error: { details?: unknown } | null,
): { port: number; pid: number; exe: string | null } | null {
  const details = error?.details;
  if (!isRecord(details)) return null;
  const args = isRecord(details.args) ? details.args : {};
  const pick = (name: string): unknown => details[name] ?? args[name];
  const port = pick("port");
  const pid = pick("pid");
  const exe = pick("exe");
  if (typeof port !== "number" || typeof pid !== "number") return null;
  return { port, pid, exe: typeof exe === "string" ? exe : null };
}

/** Installed versions of a tool as `tool@version` specs, newest first as the daemon lists them. */
export function installedSpecs(tool: ToolDto | undefined): string[] {
  if (tool === undefined) return [];
  return tool.installed.map((v) => `${tool.id}@${v.version}`);
}
