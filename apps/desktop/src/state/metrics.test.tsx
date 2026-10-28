// Acceptance (M2-E1-T9 #4): useMetricsPoll makes 0 calls while hidden, unfocused or
// unmounted, resumes within 2 s, and req/s from 100 → 140 over 2 s is 20.0.
import { act, render, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type * as T from "../ipc/generated";
import {
  METRICS_INTERVAL_MS,
  METRICS_SAMPLES,
  ingestMetrics,
  requestRate,
  statusTotals,
} from "./metrics";
import {
  ForgeStateProvider,
  useInstanceMetrics,
  useMetricsPoll,
  usePoolMetrics,
  useStatusTotals,
} from "./react";
import { IDS, daemonStatus, instance, pool, startHarness, type Harness } from "./testFixtures";

let harness: Harness | null = null;
afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  harness = null;
});

function wrapper(h: Harness) {
  return ({ children }: { children: ReactNode }) => (
    <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
  );
}

function Consumer() {
  useMetricsPoll();
  return null;
}

async function advance(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe("requestRate", () => {
  it("is the requests_total delta per second", () => {
    expect(requestRate({ t: 0, requests: 100 }, 2000, 140)).toBe(20);
    expect(requestRate(undefined, 2000, 140)).toBeNull();
    expect(requestRate({ t: 0, requests: 500 }, 2000, 10)).toBeNull(); // pool restarted
    expect(requestRate({ t: 0, requests: null }, 2000, 10)).toBeNull();
  });
});

describe("statusTotals", () => {
  it("adds the daemon itself to the stack", () => {
    const totals = statusTotals(
      {
        instances: [
          instance(IDS.mysql, "mysql/default", { state: "running", cpu_pct: 2, rss_bytes: 300 }),
          instance(IDS.redis, "redis/default", { state: "running", cpu_pct: 1, rss_bytes: 100 }),
        ],
        pools: [pool(IDS.php83, { rss_bytes: 50 })],
      },
      daemonStatus({ cpu_pct: 0.5, rss_bytes: 1000 }),
    );
    expect(totals).toEqual({
      daemonCpu: 0.5,
      daemonRss: 1000,
      stackCpu: 3,
      stackRss: 450,
      cpu: 3.5,
      rss: 1450,
      running: 3,
    });
  });

  it("keeps a 30-sample ring and drops entities that are gone", () => {
    let slice = { instances: {}, pools: {}, totals: null, lastPollAt: null };
    const status: T.ServiceStatusDto = {
      instances: [instance(IDS.mysql, "mysql/default", { cpu_pct: 1 })],
      pools: [],
    };
    for (let i = 0; i < 40; i += 1)
      slice = { ...ingestMetrics(slice, status, null, i * 2000), totals: null, lastPollAt: null };
    expect(METRICS_SAMPLES).toBe(30);
    const ring = (slice as { instances: Record<string, unknown[]> }).instances[IDS.mysql];
    expect(ring).toHaveLength(30);
    const gone = ingestMetrics(slice, { instances: [], pools: [] }, null, 100_000);
    expect(gone.instances).toEqual({});
  });
});

describe("useMetricsPoll", () => {
  it("polls every 2 s only while mounted, visible and focused", async () => {
    vi.useFakeTimers();
    harness = await startHarness();
    const h = harness;
    const base = h.count("services.status"); // the initial snapshot
    expect(METRICS_INTERVAL_MS).toBe(2000);

    // Not mounted: nothing.
    await advance(10_000);
    expect(h.count("services.status")).toBe(base);

    const view = render(<Consumer />, { wrapper: wrapper(h) });
    await advance(0);
    expect(h.count("services.status")).toBe(base + 1); // at once: no sample yet
    await advance(2000);
    expect(h.count("services.status")).toBe(base + 2);

    // Hidden: 0 calls.
    act(() => {
      h.page.set({ visible: false });
    });
    const hidden = h.count("services.status");
    await advance(10_000);
    expect(h.count("services.status")).toBe(hidden);

    // Visible again: within 2 s (here at once, the last sample is old).
    act(() => {
      h.page.set({ visible: true });
    });
    await advance(2000);
    expect(h.count("services.status")).toBeGreaterThan(hidden);
    expect(h.count("services.status") - hidden).toBeLessThanOrEqual(2);

    // Unfocused: 0 calls.
    act(() => {
      h.page.set({ focused: false });
    });
    const blurred = h.count("services.status");
    await advance(10_000);
    expect(h.count("services.status")).toBe(blurred);
    act(() => {
      h.page.set({ focused: true });
    });
    await advance(2000);
    expect(h.count("services.status")).toBeGreaterThan(blurred);

    // Unmounted: 0 calls.
    view.unmount();
    const unmounted = h.count("services.status");
    await advance(10_000);
    expect(h.count("services.status")).toBe(unmounted);
    expect(h.count("daemon.status")).toBe(1); // only the snapshot: no consumer asked for totals
  });

  it("resumes within 2 s of a recent sample, not before the interval", async () => {
    vi.useFakeTimers();
    harness = await startHarness();
    const h = harness;
    render(<Consumer />, { wrapper: wrapper(h) });
    await advance(0);
    const first = h.count("services.status");
    await advance(500);
    act(() => {
      h.page.set({ visible: false });
    });
    act(() => {
      h.page.set({ visible: true });
    });
    await advance(1499);
    expect(h.count("services.status")).toBe(first);
    await advance(1);
    expect(h.count("services.status")).toBe(first + 1);
  });

  it("does not poll while disconnected", async () => {
    vi.useFakeTimers();
    harness = await startHarness();
    const h = harness;
    act(() => {
      h.fake.disconnect();
    });
    render(<Consumer />, { wrapper: wrapper(h) });
    const base = h.count("services.status");
    await advance(10_000);
    expect(h.count("services.status")).toBe(base);
  });

  it("derives pool req/s: 100 → 140 over 2 s = 20.0", async () => {
    vi.useFakeTimers();
    harness = await startHarness();
    const h = harness;
    h.data.pools = [pool(IDS.php83, { requests_total: 100 })];
    const { result } = renderHook(() => usePoolMetrics(IDS.php83), { wrapper: wrapper(h) });
    await advance(0);
    expect(result.current).toHaveLength(1);
    expect(result.current[0]?.rps).toBeNull();
    h.data.pools = [pool(IDS.php83, { requests_total: 140 })];
    await advance(2000);
    expect(result.current).toHaveLength(2);
    expect(result.current[1]?.rps).toBe(20);
    expect(result.current[1]?.rps?.toFixed(1)).toBe("20.0");
  });

  it("feeds instance rings and the status-bar totals (daemon + stack)", async () => {
    vi.useFakeTimers();
    harness = await startHarness();
    const h = harness;
    const { result } = renderHook(
      () => ({ cpu: useInstanceMetrics(IDS.mysql), totals: useStatusTotals() }),
      { wrapper: wrapper(h) },
    );
    await advance(0);
    expect(result.current.cpu.map((s) => s.cpu)).toEqual([2]);
    expect(result.current.totals).toMatchObject({
      daemonCpu: 1.5,
      daemonRss: 40_000_000,
      stackCpu: 2,
    });
    expect(h.count("daemon.status")).toBe(2); // snapshot + the totals poll
  });
});
