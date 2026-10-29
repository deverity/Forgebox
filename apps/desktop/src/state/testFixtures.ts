// DTO builders and a fake scenario with every snapshot method answered, for the state-layer
// tests (imported only by `*.test.ts(x)`; never by app code).
import { act } from "@testing-library/react";
import { createFakeBridge, type FakeBridge, type FakeScenario } from "../ipc/fakeBridge";
import type * as T from "../ipc/generated";
import type { FrameClock } from "./frame";
import { StateLayer } from "./layer";
import { manualPageActivity, type ManualPageActivity } from "./page";
import type { ContextRuntime, RuntimeOptions } from "./runtime";

export const IDS = {
  mysql: "01JINST000000000000000MYSQ",
  redis: "01JINST000000000000000REDS",
  php83: "01JVERS000000000000000PH83",
  site: "01JSITE00000000000000000LJA",
  site2: "01JSITE00000000000000000BLG",
} as const;

export function instance(
  id: string,
  name: string,
  patch: Partial<T.InstanceDto> = {},
): T.InstanceDto {
  const [tool = name, inst = "default"] = name.split("/");
  return {
    id,
    tool,
    version_id: `01JVERS0000000000000${tool.toUpperCase().padEnd(6, "0").slice(0, 6)}`,
    version: "1.0.0",
    name: inst,
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
  };
}

export function pool(versionId: string, patch: Partial<T.PoolDto> = {}): T.PoolDto {
  return {
    series: "8.3",
    version: "8.3.36",
    version_id: versionId,
    state: "running",
    upstream: "php_8_3",
    workers: 2,
    busy_workers: 0,
    min_workers: 1,
    max_workers: 8,
    ports: [9100, 9101],
    sites: 1,
    xdebug: false,
    requests_total: 0,
    rss_bytes: 20_000_000,
    ...patch,
  };
}

export function site(id: string, host: string, patch: Partial<T.SiteDto> = {}): T.SiteDto {
  return {
    id,
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
    created_at: "2026-10-28T10:00:00Z",
    updated_at: "2026-10-28T10:00:00Z",
    ...patch,
  };
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
  };
}

export const DNS_STATUS: T.DnsStatusDto = {
  enabled: false,
  server_running: false,
  listen: null,
  tlds: ["test"],
  policy: "not_applied",
};

export const CA_STATUS: T.CaStatusDto = {
  exists: true,
  subject: "Forgebox Local CA",
  thumbprint_sha256: "AB",
  not_after: "2036-01-01T00:00:00Z",
  trusted_current_user: true,
  trusted_local_machine: false,
  name_constraints: ["test"],
};

export function daemonStatus(patch: Partial<T.DaemonStatusDto> = {}): T.DaemonStatusDto {
  return {
    version: "0.2.0",
    api_version: { major: 1, minor: 1 },
    platform: "windows",
    pid: 4242,
    started_at: "2026-10-28T09:00:00Z",
    uptime_secs: 60,
    root: "C:/Forgebox",
    portable: false,
    last_event_seq: 0,
    active_jobs: 0,
    connections: 1,
    cpu_pct: 1.5,
    rss_bytes: 40_000_000,
    ...patch,
  };
}

/** Mutable data behind the snapshot handlers (tests change it between calls). */
export interface ScenarioData {
  instances: T.InstanceDto[];
  pools: T.PoolDto[];
  sites: T.SiteDto[];
  tools: T.ToolDto[];
  daemon: T.DaemonStatusDto;
}

export function populatedData(): ScenarioData {
  return {
    instances: [
      instance(IDS.mysql, "mysql/default", {
        state: "running",
        pid: 100,
        cpu_pct: 2,
        rss_bytes: 300,
      }),
      instance(IDS.redis, "redis/default", { state: "stopped" }),
    ],
    pools: [pool(IDS.php83)],
    sites: [site(IDS.site, "loja.test")],
    tools: [tool("mysql", { current: "8.4.3", has_service: true })],
    daemon: daemonStatus(),
  };
}

/** A scenario answering every snapshot method from `data`. */
export function stateScenario(data: ScenarioData, extra: FakeScenario = {}): FakeScenario {
  return {
    name: "state",
    ...extra,
    handlers: {
      "services.status": () => ({ instances: data.instances, pools: data.pools }),
      "sites.list": () => ({ sites: data.sites }),
      "tools.list": () => ({ tools: data.tools }),
      "net.dns_status": () => DNS_STATUS,
      "tls.ca_status": () => CA_STATUS,
      "daemon.status": () => data.daemon,
      ...extra.handlers,
    },
  };
}

/** Frames on a 0 ms timer (works with real and fake timers). */
export function timerFrameClock(): FrameClock {
  return {
    request: (callback) => setTimeout(callback, 0),
    cancel: (handle) => {
      clearTimeout(handle as ReturnType<typeof setTimeout>);
    },
  };
}

export interface Harness {
  fake: FakeBridge;
  layer: StateLayer;
  runtime: ContextRuntime;
  page: ManualPageActivity;
  data: ScenarioData;
  /** Snapshot calls of `method` so far. */
  count(method: string): number;
}

/** A started runtime over a fake bridge, snapshots loaded. */
export async function startHarness(
  options: RuntimeOptions & { scenario?: FakeScenario; data?: ScenarioData } = {},
): Promise<Harness> {
  const data = options.data ?? populatedData();
  const fake = createFakeBridge(stateScenario(data, options.scenario));
  const page = manualPageActivity();
  const layer = new StateLayer(fake, {
    page,
    frame: timerFrameClock(),
    rttIntervalMs: 0,
    snapshotDebounceMs: 10,
    invalidateDebounceMs: 10,
    ...options,
  });
  const runtime = layer.runtime("local");
  await act(async () => {
    await runtime.start();
    await runtime.settled();
  });
  return {
    fake,
    layer,
    runtime,
    page,
    data,
    count: (method) => fake.calls.filter((call) => call.method === method).length,
  };
}
