// Acceptance M2-E5-T17: each rule of docs/14 section 5 produces exactly one Atenção item with its
// cause, action and `forge` command; healthy fixtures produce none.
import { describe, expect, it } from "vitest";
import type * as T from "../../../ipc/generated";
import {
  DISK_FREE_LIMIT,
  ERROR_LIMIT_PCT,
  MIN_REQUESTS,
  P99_LIMIT_MS,
  POOL_RAM_LIMIT,
  diskItems,
  poolRamItems,
  siteTrafficItems,
} from "../attention";

function stat(host: string, patch: Partial<T.SiteStatsDto> = {}): T.SiteStatsDto {
  return {
    site_id: `id-${host}`,
    host,
    window: "5m",
    at: "2026-11-04T12:00:00Z",
    requests: 200,
    rps: 1,
    p50_ms: 30,
    p90_ms: 90,
    p99_ms: 200,
    error_pct: 0.5,
    cpu_pct: 2,
    mem_bytes: 1000,
    approx: false,
    sampled: false,
    ...patch,
  };
}

function pool(patch: Partial<T.PoolDto> = {}): T.PoolDto {
  return { series: "8.3", rss_bytes: 200 * 1024 ** 2, ...patch } as T.PoolDto;
}

function machine(available: number): T.MachineStatsDto {
  return {
    at: "2026-11-04T12:00:00Z",
    cpu_pct: 10,
    logical_cpus: 8,
    mem_total: 16 * 1024 ** 3,
    mem_available: 8 * 1024 ** 3,
    forgebox_cpu_pct: 1,
    forgebox_rss: 1,
    disks: [{ role: "root", total: 500 * 1024 ** 3, free: available, available }],
  };
}

describe("performance alert rules", () => {
  it("p99 above 800 ms gives one item with the stats command", () => {
    const items = siteTrafficItems([stat("slow.test", { p99_ms: P99_LIMIT_MS + 1 })]);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      id: "p99:id-slow.test",
      severity: "warning",
      ns: "metrics",
      cli: "forge stats slow.test --window 5m",
      href: "/sites/id-slow.test",
      search: { tab: "performance" },
    });
    expect(items[0]?.cause?.key).toBe("attention.p99.cause");
    expect(items[0]?.actionLabel).toBeDefined();
  });

  it("error rate above 5 % gives one critical item", () => {
    const items = siteTrafficItems([stat("bad.test", { error_pct: ERROR_LIMIT_PCT + 0.1 })]);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      id: "errors:id-bad.test",
      severity: "critical",
      cli: "forge stats bad.test --window 5m",
    });
  });

  it("a site that trips both rules gets two items, one per rule", () => {
    const items = siteTrafficItems([stat("both.test", { p99_ms: 1500, error_pct: 12 })]);
    expect(items.map((item) => item.id)).toEqual(["p99:id-both.test", "errors:id-both.test"]);
  });

  it("pool RAM above 1 GB gives one item with forge top", () => {
    const items = poolRamItems([pool({ rss_bytes: POOL_RAM_LIMIT + 1 }), pool({ series: "8.2" })]);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ id: "pool-ram:8.3", cli: "forge top", href: "/php" });
  });

  it("free disk below 2 GB gives one critical item per volume with forge top", () => {
    const items = diskItems(machine(DISK_FREE_LIMIT - 1));
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ id: "disk:root", severity: "critical", cli: "forge top" });
    expect(items[0]?.title.key).toBe("attention.disk.title.root");
  });

  it("healthy fixtures produce nothing, right at every limit", () => {
    expect(
      siteTrafficItems([
        stat("ok.test"),
        stat("edge.test", { p99_ms: P99_LIMIT_MS, error_pct: ERROR_LIMIT_PCT }),
        stat("none.test", { p99_ms: null, error_pct: null }),
      ]),
    ).toEqual([]);
    expect(poolRamItems([pool({ rss_bytes: POOL_RAM_LIMIT })])).toEqual([]);
    expect(diskItems(machine(DISK_FREE_LIMIT))).toEqual([]);
    expect(diskItems(null)).toEqual([]);
    expect(diskItems(undefined)).toEqual([]);
  });

  it("stays quiet on too little traffic to judge", () => {
    expect(
      siteTrafficItems([
        stat("quiet.test", { requests: MIN_REQUESTS - 1, p99_ms: 5000, error_pct: 100 }),
      ]),
    ).toEqual([]);
  });
});
