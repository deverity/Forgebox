import { describe, expect, it } from "vitest";
import type { PhpExtDto, PhpVersionDto } from "../../ipc/generated";
import { ingestMetrics } from "../../state/metrics";
import { createContextStores } from "../../state/stores";
import {
  filterExtensions,
  indexExtensions,
  isCliVersion,
  latestRate,
  parseXdebugPort,
  seriesColumns,
  shownEnabled,
  variantBadges,
} from "./model";

function version(series: string, exact: string, patch: Partial<PhpVersionDto> = {}) {
  return {
    version_id: `id-${exact}`,
    version: exact,
    series,
    thread_safety: "nts",
    compiler: "vs17",
    arch: "x64",
    is_current_cli: false,
    pool: null,
    sites: 0,
    xdebug: "off",
    ...patch,
  } satisfies PhpVersionDto;
}

describe("seriesColumns", () => {
  it("orders the series from the oldest to the newest, one column each", () => {
    const columns = seriesColumns([
      version("8.4", "8.4.1"),
      version("7.4", "7.4.33"),
      version("8.10", "8.10.0"),
      version("8.2", "8.2.30"),
    ]);
    expect(columns.map((c) => c.series)).toEqual(["7.4", "8.2", "8.4", "8.10"]);
  });

  it("keeps the CLI version, else the newest patch, when a series has several", () => {
    expect(
      seriesColumns([version("8.3", "8.3.9"), version("8.3", "8.3.36")]).map((c) => c.version),
    ).toEqual(["8.3.36"]);
    expect(
      seriesColumns([
        version("8.3", "8.3.9", { is_current_cli: true }),
        version("8.3", "8.3.36"),
      ]).map((c) => c.version),
    ).toEqual(["8.3.9"]);
  });
});

describe("variantBadges / isCliVersion", () => {
  it("names the build variant", () => {
    expect(variantBadges(version("8.3", "8.3.36"))).toEqual(["NTS", "vs17", "x64"]);
    expect(variantBadges({ thread_safety: "ts", compiler: null, arch: "x64" })).toEqual([
      "TS",
      "x64",
    ]);
  });

  it("follows the tools store, with the list as a fallback", () => {
    const v = version("8.4", "8.4.1", { is_current_cli: false });
    expect(isCliVersion(v, undefined)).toBe(false);
    expect(isCliVersion(v, { current: null })).toBe(false);
    expect(isCliVersion({ ...v, is_current_cli: true }, { current: null })).toBe(true);
    expect(isCliVersion(v, { current: "8.4.1" })).toBe(true);
    // `tool.current_changed` carries the folder name until the refetch.
    expect(isCliVersion(v, { current: "8.4.1-nts-vs17-x64" })).toBe(true);
    expect(isCliVersion(v, { current: "8.4.10" })).toBe(false);
    expect(isCliVersion(v, { current: "8.3.36" })).toBe(false);
  });
});

describe("extensions", () => {
  const extensions: PhpExtDto[] = Array.from({ length: 60 }, (_, i) => ({
    name: i === 7 ? "intl" : i === 8 ? "pdo_mysql" : `ext_${String(i).padStart(2, "0")}`,
    enabled: i % 2 === 0,
    bundled: i % 3 !== 0,
    zend: i === 9,
  }));

  it("searches by name, folding case and accents", () => {
    const index = indexExtensions(extensions);
    expect(filterExtensions(index, "INT").map((e) => e.extension.name)).toEqual(["intl"]);
    expect(filterExtensions(index, "pdo mysql")).toHaveLength(1);
    expect(filterExtensions(index, "pdo_my").map((e) => e.extension.name)).toEqual(["pdo_mysql"]);
    expect(filterExtensions(index, "  ")).toHaveLength(60);
  });

  it("filters 60 extensions in under 16 ms (median of 200 runs)", () => {
    const index = indexExtensions(extensions);
    const runs: number[] = [];
    for (let i = 0; i < 200; i += 1) {
      const start = performance.now();
      filterExtensions(index, "ext_5");
      runs.push(performance.now() - start);
    }
    runs.sort((a, b) => a - b);
    expect(runs[100] ?? Infinity).toBeLessThan(16);
    expect(runs[199] ?? Infinity).toBeLessThan(16);
  });

  it("shows the optimistic value while the job runs", () => {
    expect(shownEnabled({ name: "intl", enabled: true }, {})).toBe(true);
    expect(shownEnabled({ name: "intl", enabled: true }, { intl: false })).toBe(false);
    expect(shownEnabled({ name: "intl", enabled: true }, { other: false })).toBe(true);
  });
});

describe("parseXdebugPort", () => {
  it("keeps the saved port when blank and refuses what is not a port", () => {
    expect(parseXdebugPort("")).toBeNull();
    expect(parseXdebugPort(" 9010 ")).toBe(9010);
    expect(parseXdebugPort("0")).toBeUndefined();
    expect(parseXdebugPort("65536")).toBeUndefined();
    expect(parseXdebugPort("90a")).toBeUndefined();
  });
});

describe("pool req/s", () => {
  it("is the requests_total delta over the poll interval: 100 to 140 in 2 s is 20", () => {
    const pool = {
      series: "8.3",
      version: "8.3.36",
      version_id: "pool-1",
      state: "running" as const,
      upstream: "php_8_3",
      workers: 4,
      busy_workers: 1,
      min_workers: 2,
      max_workers: 8,
      ports: [9100],
      sites: 1,
      xdebug: false,
      requests_total: 100,
      rss_bytes: 1000,
    };
    const empty = createContextStores("local").metrics.getState();
    const first = ingestMetrics(empty, { instances: [], pools: [pool] }, null, 0);
    expect(latestRate(first.pools["pool-1"] ?? [])).toBeNull();
    const second = ingestMetrics(
      first,
      { instances: [], pools: [{ ...pool, requests_total: 140 }] },
      null,
      2000,
    );
    expect(latestRate(second.pools["pool-1"] ?? [])).toBe(20);
  });
});
