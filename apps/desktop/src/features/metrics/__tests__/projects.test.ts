// Acceptance M2-E5-T17 (pure parts): sorting by each metric, null handling, and idle detection
// at the 6 d / 8 d boundary with the suggested stop command.
import { describe, expect, it } from "vitest";
import type * as T from "../../../ipc/generated";
import { IDLE_DAYS, idleSites, seriesValue, sortProjects } from "../projects";

const NOW = Date.parse("2026-11-04T12:00:00Z");
const DAY = 24 * 60 * 60 * 1000;

function stat(host: string, patch: Partial<T.SiteStatsDto> = {}): T.SiteStatsDto {
  return {
    site_id: `id-${host}`,
    host,
    window: "24h",
    at: new Date(NOW).toISOString(),
    requests: 100,
    rps: 1,
    p50_ms: 10,
    p90_ms: 20,
    p99_ms: 40,
    error_pct: 0,
    cpu_pct: 1,
    mem_bytes: 1000,
    approx: false,
    sampled: false,
    ...patch,
  };
}

const FLEET = [
  stat("a.test", { cpu_pct: 5, mem_bytes: 300, p99_ms: 900, error_pct: 0.1 }),
  stat("b.test", { cpu_pct: 20, mem_bytes: 100, p99_ms: 100, error_pct: 7 }),
  stat("c.test", { cpu_pct: 1, mem_bytes: 900, p99_ms: 400, error_pct: 3 }),
  stat("d.test", { cpu_pct: null, mem_bytes: null, p99_ms: null, error_pct: null }),
];

const hosts = (sites: readonly T.SiteStatsDto[]) => sites.map((site) => site.host);

describe("sortProjects", () => {
  it.each([
    ["cpu", ["b.test", "a.test", "c.test", "d.test"]],
    ["mem", ["c.test", "a.test", "b.test", "d.test"]],
    ["p99", ["a.test", "c.test", "b.test", "d.test"]],
    ["errors", ["b.test", "c.test", "a.test", "d.test"]],
  ] as const)("ranks by %s, highest first, sites without a value last", (key, expected) => {
    expect(hosts(sortProjects(FLEET, key))).toEqual(expected);
  });

  it("reverses on ascending but still keeps sites without a value last", () => {
    expect(hosts(sortProjects(FLEET, "cpu", "asc"))).toEqual([
      "c.test",
      "a.test",
      "b.test",
      "d.test",
    ]);
  });

  it("breaks ties by host and does not mutate its input", () => {
    const tied = [stat("z.test"), stat("m.test"), stat("a.test")];
    const before = hosts(tied);
    expect(hosts(sortProjects(tied, "cpu"))).toEqual(["a.test", "m.test", "z.test"]);
    expect(hosts(tied)).toEqual(before);
  });
});

describe("seriesValue", () => {
  it("follows the sort key", () => {
    const point = {
      cpu_pct: 1,
      mem_bytes: 2,
      p99_ms: 3,
      error_pct: 4,
    } as T.SeriesPointDto;
    expect(["cpu", "mem", "p99", "errors"].map((key) => seriesValue(point, key as "cpu"))).toEqual([
      1, 2, 3, 4,
    ]);
  });
});

describe("idleSites", () => {
  const ago = (days: number) => new Date(NOW - days * DAY).toISOString();

  it("flags 8 days without requests and keeps 6 days out", () => {
    const sites = [
      stat("six.test", { last_request_at: ago(6), processes: 1 }),
      stat("eight.test", { last_request_at: ago(8), processes: 2 }),
    ];
    const idle = idleSites(sites, NOW);
    expect(idle.map((entry) => entry.site.host)).toEqual(["eight.test"]);
    expect(idle[0]?.idleDays).toBe(8);
    expect(IDLE_DAYS).toBe(7);
  });

  it("suggests the stop command of the web process", () => {
    const [entry] = idleSites([stat("loja.test", { last_request_at: ago(9), processes: 1 })], NOW);
    expect(entry?.stopCli).toBe("forge run loja.test web stop");
  });

  it("ignores idle sites with nothing running", () => {
    const sites = [
      stat("none.test", { last_request_at: ago(30), processes: 0 }),
      stat("absent.test", { last_request_at: ago(30) }),
    ];
    expect(idleSites(sites, NOW)).toEqual([]);
  });

  it("treats no recorded request as idle, listed first, longest idle next", () => {
    const sites = [
      stat("ten.test", { last_request_at: ago(10), processes: 1 }),
      stat("never.test", { last_request_at: null, processes: 1 }),
      stat("twenty.test", { last_request_at: ago(20), processes: 1 }),
    ];
    const idle = idleSites(sites, NOW);
    expect(idle.map((entry) => entry.site.host)).toEqual(["never.test", "twenty.test", "ten.test"]);
    expect(idle[0]?.idleDays).toBeNull();
  });
});
