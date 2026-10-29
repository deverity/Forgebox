// DTO builders for the QA scenarios. Every builder returns a generated.ts type (checked with
// `satisfies`), so a DTO change in forge-ipc breaks `tsc` here (M2-E1-T19). QA only: imported
// through src/qa/bootstrap.ts, which production builds never reach.
import type * as T from "../../../src/ipc/generated";

const NOW = "2026-10-28T10:00:00Z";

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/**
 * Deterministic ULID-shaped ids (26 Crockford chars) so screenshots and tests are stable.
 * `n` is encoded digit by digit in base 32, so distinct (prefix, n) pairs never collide.
 */
export function fakeId(prefix: string, n: number): string {
  const width = 26 - prefix.length;
  let rest = Math.max(0, Math.floor(n));
  let body = "";
  for (let i = 0; i < width; i += 1) {
    body = CROCKFORD.charAt(rest % 32) + body;
    rest = Math.floor(rest / 32);
  }
  return prefix + body;
}

export function instance(
  n: number,
  tool: string,
  name: string,
  patch: Partial<T.InstanceDto> = {},
): T.InstanceDto {
  return {
    id: fakeId("01QA1NST", n),
    tool,
    version_id: fakeId("01QAVERS", n % 7),
    version: "1.0.0",
    name,
    state: "stopped",
    pid: null,
    ports: [],
    autostart: false,
    started_at: null,
    cpu_pct: null,
    rss_bytes: null,
    restarts: 0,
    last_error: null,
    ...patch,
  } satisfies T.InstanceDto;
}

export function port(owner: T.InstanceDto, number: number, name: string): T.PortDto {
  return {
    id: fakeId("01QAP0RT", number),
    port: number,
    protocol: "tcp",
    owner_kind: "instance",
    owner_id: owner.id,
    owner_name: `${owner.tool}/${owner.name}`,
    name,
    state: "bound",
    conflict: null,
  } satisfies T.PortDto;
}

export function site(n: number, host: string, patch: Partial<T.SiteDto> = {}): T.SiteDto {
  return {
    id: fakeId("01QAS1TE", n),
    slug: host.split(".")[0] ?? host,
    host,
    aliases: [],
    kind: "php",
    root: `C:/Forgebox/www/${host}`,
    driver: null,
    server: "nginx",
    php: { series: "8.3", exact: null, resolved: "8.3.36", upstream: "php_8_3" },
    upstream: null,
    websocket: false,
    secure: true,
    http_redirect: true,
    env: {},
    xdebug: "inherit",
    parked: false,
    enabled: true,
    case_sensitive: true,
    url: `https://${host}`,
    processes: [],
    created_at: NOW,
    updated_at: NOW,
    ...patch,
  } satisfies T.SiteDto;
}

export function tool(id: string, patch: Partial<T.ToolDto> = {}): T.ToolDto {
  return {
    id,
    name: id,
    category: null,
    description: null,
    installed: [],
    current: null,
    policy: "notify",
    latest: null,
    update_available: null,
    has_service: false,
    ...patch,
  } satisfies T.ToolDto;
}

export const DNS_STATUS = {
  enabled: false,
  server_running: false,
  listen: null,
  tlds: ["test"],
  policy: "not_applied",
} satisfies T.DnsStatusDto;

export const CA_STATUS = {
  exists: true,
  subject: "Forgebox Local CA",
  thumbprint_sha256: "AB",
  not_after: "2036-01-01T00:00:00Z",
  trusted_current_user: true,
  trusted_local_machine: false,
  name_constraints: ["test"],
} satisfies T.CaStatusDto;

export function daemonStatus(patch: Partial<T.DaemonStatusDto> = {}): T.DaemonStatusDto {
  return {
    version: "0.2.0",
    api_version: { major: 1, minor: 1 },
    platform: "windows",
    pid: 4242,
    started_at: NOW,
    uptime_secs: 60,
    root: "C:/Forgebox",
    portable: false,
    last_event_seq: 0,
    active_jobs: 0,
    connections: 1,
    cpu_pct: 1.5,
    rss_bytes: 40_000_000,
    ...patch,
  } satisfies T.DaemonStatusDto;
}

/** What the snapshot handlers answer. */
export interface QaData {
  instances: T.InstanceDto[];
  sites: T.SiteDto[];
  tools: T.ToolDto[];
  daemon: T.DaemonStatusDto;
}

export function emptyData(): QaData {
  return { instances: [], sites: [], tools: [], daemon: daemonStatus() };
}

export function populatedData(): QaData {
  const mysql = instance(1, "mysql", "default", { state: "running", pid: 100, cpu_pct: 2 });
  const mailpit = instance(3, "mailpit", "default", { state: "running", pid: 101 });
  mysql.ports = [port(mysql, 3306, "main")];
  mailpit.ports = [port(mailpit, 8025, "ui"), port(mailpit, 1025, "smtp")];
  return {
    instances: [mysql, instance(2, "redis", "default"), mailpit],
    sites: [
      site(1, "loja.test"),
      site(2, "blog.test", { secure: false, url: "http://blog.test" }),
      site(3, "api.test", { kind: "proxy", upstream: "http://127.0.0.1:3000" }),
    ],
    tools: [
      tool("mysql", { current: "8.4.3", has_service: true }),
      tool("php", { current: "8.3.36" }),
    ],
    daemon: daemonStatus(),
  };
}

export function heavyData(sites = 500, instances = 50): QaData {
  return {
    instances: Array.from({ length: instances }, (_, i) =>
      instance(i + 1, `tool${String(i % 10)}`, `inst${String(i)}`, {
        state: i % 3 === 0 ? "running" : "stopped",
        pid: i % 3 === 0 ? 1000 + i : null,
        cpu_pct: i % 3 === 0 ? (i % 17) + 0.5 : null,
        rss_bytes: i % 3 === 0 ? 20_000_000 + i * 1000 : null,
      }),
    ),
    sites: Array.from({ length: sites }, (_, i) => site(i + 1, `site${String(i)}.test`)),
    tools: Array.from({ length: 12 }, (_, i) => tool(`tool${String(i)}`)),
    daemon: daemonStatus(),
  };
}

/** One `service.*` event payload for an instance. */
export function servicePayload(inst: T.InstanceDto, state: T.ServiceState): T.ServicePayload {
  return {
    v: 1,
    instance_id: inst.id,
    name: `${inst.tool}/${inst.name}`,
    tool: inst.tool,
    state,
    pid: state === "running" ? 2000 : null,
    strategy: null,
    exit_code: null,
    error: null,
  } satisfies T.ServicePayload;
}
