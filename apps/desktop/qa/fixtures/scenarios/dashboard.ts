// Scenarios of the Dashboard (M2-E2-T14): `dashboard` is healthy (no Atenção), `dashboard-attention`
// trips every source once. Both carry a PHP pool, a few installed tools and an event history for
// Atividade. The built-in `empty` scenario covers the empty state.
import type { FakeScenario } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import { CA_STATUS, fakeId, heavyData, populatedData, type QaData } from "./data";
import { metricsHandlers } from "./metrics";

const DAY = 24 * 60 * 60 * 1000;
const inDays = (days: number) => new Date(Date.now() + days * DAY).toISOString();

function pool(): T.PoolDto {
  return {
    series: "8.3",
    version: "8.3.36",
    version_id: fakeId("01QAVERS", 3),
    state: "running",
    upstream: "php_8_3",
    workers: 2,
    busy_workers: 1,
    min_workers: 1,
    max_workers: 8,
    ports: [9100, 9101],
    sites: 2,
    xdebug: false,
    requests_total: 1200,
    rss_bytes: 48_000_000,
  } satisfies T.PoolDto;
}

function history(): { kind: string; payload: unknown }[] {
  return [
    {
      kind: "tool.install.completed",
      payload: {
        v: 1,
        job_id: null,
        tool: "php",
        version: "8.3.36",
        version_id: null,
        progress: null,
        error: null,
      },
    },
    { kind: "site.created", payload: { v: 1, site_id: fakeId("01QAS1TE", 1), host: "loja.test" } },
    { kind: "site.created", payload: { v: 1, site_id: fakeId("01QAS1TE", 2), host: "blog.test" } },
    {
      kind: "tool.install.completed",
      payload: {
        v: 1,
        job_id: null,
        tool: "mysql",
        version: "8.4.3",
        version_id: null,
        progress: null,
        error: null,
      },
    },
  ];
}

function baseData(): QaData {
  // populatedData already carries mysql, redis, mailpit and php.
  return populatedData();
}

export function healthyDashboard(): { data: QaData; extra: FakeScenario } {
  const data = baseData();
  return {
    data,
    extra: {
      events: history(),
      handlers: {
        "services.status": () => ({ instances: data.instances, pools: [pool()] }),
        // The catalog handlers (spread after the base ones) would answer their own tool list.
        "tools.list": () => ({ tools: data.tools }),
        "net.dns_status": () => ({
          enabled: true,
          server_running: true,
          listen: "127.0.0.1:53",
          tlds: ["test"],
          policy: "applied",
          mode: "nrpt",
        }),
      },
    },
  };
}

export function attentionDashboard(): { data: QaData; extra: FakeScenario } {
  const { data, extra } = healthyDashboard();
  const redis = data.instances[1];
  if (redis !== undefined) redis.state = "failed";
  // `tool.updates_available` is live-only: right after opening, `tools.list` carries the news.
  const php = data.tools.find((candidate) => candidate.id === "php");
  if (php !== undefined) php.update_available = "8.3.37";
  return {
    data,
    extra: {
      events: [
        ...history(),
        {
          kind: "config.drift_detected",
          payload: {
            v: 1,
            owner: "nginx",
            path: "C:/Forgebox/etc/nginx/nginx.conf",
            fs_path: null,
          },
        },
        {
          kind: "tool.updates_available",
          payload: {
            v: 1,
            count: 1,
            updates: [{ tool: "php", series: "8.3", from: "8.3.36", to: "8.3.37" }],
          },
        },
      ],
      handlers: {
        ...extra.handlers,
        "services.ports": () => ({
          ports: [],
          external: [{ port: 3306, protocol: "tcp", holder: { pid: 4321, exe: "mysqld.exe" } }],
        }),
        "tls.ca_status": () => ({ ...CA_STATUS, not_after: inDays(30) }),
        "tls.list": () => ({
          certificates: [
            {
              host: "loja.test",
              sans: ["loja.test"],
              site_id: fakeId("01QAS1TE", 1),
              not_before: inDays(-60),
              not_after: inDays(12),
              thumbprint_sha256: "AA",
            },
          ],
        }),
        "net.dns_status": () => ({
          enabled: true,
          server_running: true,
          listen: "127.0.0.1:53",
          tlds: ["test"],
          policy: "hosts_fallback",
          mode: "hosts",
          reason: "resolver_probe_failed",
        }),
      },
    },
  };
}

/** Sites of `dashboard-projects`: the ones the e2e spec and the unit fixtures look for. */
export const PROJECTS_SITES = 200;

/**
 * `dashboard-projects` (M2-E5-T17): 200 sites ranked, one slow, one failing, one idle for 8 days
 * (a 6 day one stays out), a PHP pool above 1 GB and a nearly full disk: every performance rule
 * of docs/14 section 5 trips once.
 */
export function projectsDashboard(): { data: QaData; extra: FakeScenario } {
  const data = heavyData(PROJECTS_SITES, 4);
  const day = (days: number) => new Date(Date.now() - days * DAY).toISOString();
  const handlers = metricsHandlers(data, {
    sites: {
      "site1.test": { p99_ms: 950, requests: 400 },
      "site2.test": { error_pct: 9.2, requests: 400 },
      "site3.test": { last_request_at: day(8), processes: 2, requests: 0, rps: 0 },
      "site4.test": { last_request_at: day(6), processes: 1, requests: 0, rps: 0 },
    },
    machine: {
      disks: [
        { role: "root", total: 500 * 1024 ** 3, free: 1.2 * 1024 ** 3, available: 1.2 * 1024 ** 3 },
      ],
    },
  });
  return {
    data,
    extra: {
      handlers: {
        ...handlers.handlers,
        "services.status": () => ({
          instances: data.instances,
          pools: [{ ...pool(), rss_bytes: 1.5 * 1024 ** 3 }],
        }),
      },
    },
  };
}
