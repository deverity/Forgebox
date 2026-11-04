// Acceptance M2-E5-T14: 100 items in one frame are 1 commit, a site-B sample re-renders
// nothing of site A, two consumers share one subscription and unmount closes it, a reconnect
// replaces the store with the snapshot without duplicate points, and the CLI of the pool and
// metrics actions comes from the templates.
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setSiteMetrics, setSitePoolMode, statsCli, topCli } from "../../../actions/metrics";
import type * as T from "../../../ipc/generated";
import { ForgeStateProvider } from "../../../state";
import { IDS, startHarness, type Harness } from "../../../state/testFixtures";
import type { FrameClock } from "../../../state/frame";
import { manualPageActivity } from "../../../state/page";
import { MetricsHub, applyEntries } from "../state/hub";
import {
  TOP_POLL_MS,
  useSiteMetrics,
  useSiteRoutes,
  useSiteSeries,
  useSiteStats,
  useTop,
} from "../state/hooks";
import { EMPTY_METRICS } from "../state/types";

let harness: Harness | null = null;
afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  harness = null;
});

const T0 = Date.parse("2026-11-04T12:00:00Z");

function sample(
  siteId: string,
  host: string,
  index: number,
  patch: Partial<T.SiteStatsDto> = {},
): T.SiteStatsDto {
  return {
    site_id: siteId,
    host,
    window: "5m",
    at: new Date(T0 + index * 2000).toISOString(),
    requests: index,
    rps: index / 2,
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

/** A frame clock the test advances by hand. */
function manualFrames() {
  const pending: (() => void)[] = [];
  const scheduler: FrameClock = {
    request: (callback) => pending.push(callback),
    cancel: () => {
      pending.length = 0;
    },
  };
  return {
    scheduler,
    pending: () => pending.length,
    run: () => {
      const batch = pending.splice(0);
      for (const callback of batch) callback();
    },
  };
}

function wrapperOf(h: Harness) {
  return ({ children }: { children: ReactNode }) => (
    <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
  );
}

function hubOf(h: Harness, frame: FrameClock) {
  return new MetricsHub({
    api: h.runtime.api,
    connection: h.runtime.stores.connection,
    frame,
  });
}

describe("MetricsHub", () => {
  it("applies 100 stream items of one frame in one commit", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    const frames = manualFrames();
    const hub = hubOf(h, frames.scheduler);
    const { store, release } = hub.acquire(IDS.site, "5m");
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 1);
    });
    let commits = 0;
    store.subscribe(() => {
      commits += 1;
    });
    await act(async () => {
      for (let i = 0; i < 100; i += 1) h.fake.emitMetrics(sample(IDS.site, "loja.test", i));
      await vi.waitFor(() => {
        expect(frames.pending()).toBe(1);
      });
    });
    expect(commits).toBe(0);
    frames.run();
    expect(commits).toBe(1);
    expect(store.getState().points).toHaveLength(100);
    expect(store.getState().status).toBe("live");
    release();
    hub.dispose();
  });

  it("shares one subscription between consumers and closes it on the last release", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    const hub = hubOf(h, manualFrames().scheduler);
    const a = hub.acquire(IDS.site, "5m");
    const b = hub.acquire(IDS.site, "5m");
    expect(b.store).toBe(a.store);
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 1);
    });
    expect(hub.refCount(IDS.site, "5m")).toBe(2);
    a.release();
    a.release(); // idempotent
    expect(h.fake.openStreams()).toBe(base + 1);
    b.release();
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base);
    });
    expect(hub.subscriptionCount()).toBe(0);
    hub.dispose();
  });

  it("closes a subscription that was released before it was answered", async () => {
    harness = await startHarness({ scenario: { latencyMs: 20 } });
    const h = harness;
    const base = h.fake.openStreams();
    const hub = hubOf(h, manualFrames().scheduler);
    hub.acquire(IDS.site, "5m").release();
    await new Promise((resolve) => setTimeout(resolve, 80));
    expect(h.fake.openStreams()).toBe(base);
    hub.dispose();
  });

  it("replaces the store with the snapshot after a reconnect, without duplicate points", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    const frames = manualFrames();
    const hub = hubOf(h, frames.scheduler);
    const { store, release } = hub.acquire(IDS.site, "5m");
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 1);
    });
    await act(async () => {
      for (let i = 0; i < 5; i += 1) h.fake.emitMetrics(sample(IDS.site, "loja.test", i));
      await vi.waitFor(() => {
        expect(frames.pending()).toBe(1);
      });
    });
    frames.run();
    expect(store.getState().points).toHaveLength(5);

    await act(async () => {
      h.fake.disconnect();
      await vi.waitFor(() => {
        expect(h.runtime.stores.connection.getState().status).not.toBe("connected");
      });
      h.fake.reconnect();
      await vi.waitFor(() => {
        expect(h.runtime.stores.connection.getState().status).toBe("connected");
      });
    });
    // The daemon's snapshot (the same instants again) and a new sample.
    await act(async () => {
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 3));
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 4));
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 5));
      await vi.waitFor(() => {
        expect(frames.pending()).toBe(1);
      });
    });
    frames.run();
    const state = store.getState();
    expect(state.snapshots).toBe(1);
    expect(state.points.map((p) => p.t)).toEqual([3, 4, 5].map((i) => T0 + i * 2000));
    release();
    hub.dispose();
  });

  it("does not touch a store for samples of another site", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    const frames = manualFrames();
    const hub = hubOf(h, frames.scheduler);
    const a = hub.acquire(IDS.site, "5m");
    const b = hub.acquire(IDS.site2, "5m");
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 2);
    });
    let commitsA = 0;
    a.store.subscribe(() => {
      commitsA += 1;
    });
    await act(async () => {
      h.fake.emitMetrics(sample(IDS.site2, "blog.test", 1));
      await vi.waitFor(() => {
        expect(frames.pending()).toBe(1);
      });
    });
    frames.run();
    expect(commitsA).toBe(0);
    expect(b.store.getState().points).toHaveLength(1);
    expect(a.store.getState().points).toHaveLength(0);
    a.release();
    b.release();
    hub.dispose();
  });

  it("applies samples without rAF while the page is hidden and bounds the queue", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    const frames = manualFrames();
    const page = manualPageActivity(false);
    const hub = new MetricsHub({
      api: h.runtime.api,
      connection: h.runtime.stores.connection,
      frame: frames.scheduler,
      page,
      maxQueue: 5,
    });
    const { store, release } = hub.acquire(IDS.site, "5m");
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 1);
    });
    // Hidden: nothing is requested from rAF; the 250 ms timer applies the batch.
    await act(async () => {
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 1));
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 2));
      await new Promise((resolve) => setTimeout(resolve, 400));
    });
    expect(frames.pending()).toBe(0);
    expect(store.getState().points).toHaveLength(2);
    // Overflow: past maxQueue the backlog is dropped, never grown without bound.
    await act(async () => {
      for (let i = 3; i < 13; i += 1) h.fake.emitMetrics(sample(IDS.site, "loja.test", i));
      await new Promise((resolve) => setTimeout(resolve, 400));
    });
    expect(store.getState().points.length).toBeLessThanOrEqual(10);
    // A later sample after the overflow replaces the points (snapshot semantics).
    await act(async () => {
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 20));
      await new Promise((resolve) => setTimeout(resolve, 400));
    });
    expect(store.getState().points).toHaveLength(1);
    expect(frames.pending()).toBe(0);
    release();
    hub.dispose();
  });
});

describe("applyEntries", () => {
  it("drops samples that are not newer, caps the points and counts lagged items", () => {
    const first = sample("s", "h", 1);
    const result = applyEntries(
      EMPTY_METRICS,
      [
        { kind: "sample", sample: sample("s", "h", 2) },
        { kind: "sample", sample: first },
        { kind: "sample", sample: sample("s", "h", 2) },
        { kind: "sample", sample: sample("s", "h", 3) },
        { kind: "sample", sample: sample("s", "h", 4) },
        { kind: "lagged", missed: 7 },
      ],
      false,
      3,
    );
    expect(result.state.points.map((p) => p.t)).toEqual([2, 3, 4].map((i) => T0 + i * 2000));
    expect(result.state.missed).toBe(7);
  });

  it("a reset entry makes the next sample a snapshot", () => {
    const base = applyEntries(
      EMPTY_METRICS,
      [{ kind: "sample", sample: sample("s", "h", 5) }],
      false,
    );
    const reset = applyEntries(base.state, [{ kind: "reset" }], false);
    expect(reset.awaitingSnapshot).toBe(true);
    const next = applyEntries(
      reset.state,
      [{ kind: "sample", sample: sample("s", "h", 1) }],
      reset.awaitingSnapshot,
    );
    expect(next.state.points).toHaveLength(1);
    expect(next.state.snapshots).toBe(1);
  });
});

describe("useSiteMetrics", () => {
  it("follows a site, shares the stream between hooks and unsubscribes on unmount", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    let rendersA = 0;
    const { result, unmount } = renderHook(
      () => {
        rendersA += 1;
        return {
          a: useSiteMetrics(IDS.site, "5m"),
          a2: useSiteMetrics(IDS.site, "5m"),
          b: useSiteMetrics(IDS.site2, "5m"),
        };
      },
      { wrapper: wrapperOf(h) },
    );
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 2); // site A once, site B once
    });
    act(() => {
      h.fake.emitMetrics(sample(IDS.site, "loja.test", 1));
    });
    await waitFor(() => {
      expect(result.current.a.points).toHaveLength(1);
    });
    expect(result.current.a2.latest?.host).toBe("loja.test");
    expect(result.current.b.points).toHaveLength(0);
    unmount();
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base);
    });
    expect(rendersA).toBeGreaterThan(0);
  });

  it("does not re-render a site A consumer for a site B sample", async () => {
    harness = await startHarness();
    const h = harness;
    const base = h.fake.openStreams();
    let renders = 0;
    const { result } = renderHook(
      () => {
        renders += 1;
        return useSiteMetrics(IDS.site, "5m");
      },
      { wrapper: wrapperOf(h) },
    );
    const other = renderHook(() => useSiteMetrics(IDS.site2, "5m"), { wrapper: wrapperOf(h) });
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(base + 2);
    });
    // Settle the mount renders, then count only what the site B sample causes.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    const before = renders;
    act(() => {
      h.fake.emitMetrics(sample(IDS.site2, "blog.test", 1));
    });
    await waitFor(() => {
      expect(other.result.current.points).toHaveLength(1);
    });
    expect(renders).toBe(before);
    expect(result.current.points).toHaveLength(0);
  });

  it("is idle without a site or when the daemon lacks metrics.subscribe", async () => {
    harness = await startHarness({
      scenario: { hello: { not_implemented: ["metrics.subscribe"] } },
    });
    const h = harness;
    const base = h.fake.openStreams();
    const { result } = renderHook(() => useSiteMetrics(IDS.site, "5m"), { wrapper: wrapperOf(h) });
    expect(result.current.status).toBe("idle");
    expect(h.fake.openStreams()).toBe(base);
  });
});

describe("queries", () => {
  it("calls stats.get, stats.series and stats.routes with the site and window", async () => {
    harness = await startHarness({
      scenario: {
        handlers: {
          "stats.get": () => ({ window: "1h", sites: [sample(IDS.site, "loja.test", 1)] }),
          "stats.series": () => ({
            site_id: IDS.site,
            host: "loja.test",
            range: "24h",
            step_ms: 60_000,
            from: sample(IDS.site, "loja.test", 0).at,
            to: sample(IDS.site, "loja.test", 1).at,
            points: [],
            markers: [],
          }),
          "stats.routes": () => ({
            site_id: IDS.site,
            host: "loja.test",
            since: sample(IDS.site, "loja.test", 0).at,
            at: sample(IDS.site, "loja.test", 1).at,
            total: 0,
            routes: [],
            recent_errors: [],
          }),
        },
      },
    });
    const h = harness;
    const { result } = renderHook(
      () => ({
        stats: useSiteStats(IDS.site, "1h"),
        series: useSiteSeries(IDS.site, "24h"),
        routes: useSiteRoutes(IDS.site),
      }),
      { wrapper: wrapperOf(h) },
    );
    await waitFor(() => {
      expect(result.current.stats.data?.sites).toHaveLength(1);
      expect(result.current.series.isSuccess).toBe(true);
      expect(result.current.routes.isSuccess).toBe(true);
    });
    const params = (method: string) => h.fake.calls.find((c) => c.method === method)?.params;
    expect(params("stats.get")).toEqual({ site: IDS.site, window: "1h" });
    expect(params("stats.series")).toEqual({ site: IDS.site, range: "24h" });
    expect(params("stats.routes")).toEqual({ site: IDS.site });
  });

  it("polls stats.top (5 s by default) only while the page is visible", async () => {
    harness = await startHarness({
      scenario: { handlers: { "stats.top": () => ({ processes: [] }) } },
    });
    const h = harness;
    renderHook(() => useTop({ intervalMs: 40 }), { wrapper: wrapperOf(h) });
    await waitFor(() => {
      expect(h.count("stats.top")).toBeGreaterThanOrEqual(3);
    });
    // Hidden: the polling stops.
    const hidden = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    const frozen = h.count("stats.top");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 200));
    });
    expect(h.count("stats.top")).toBe(frozen);
    hidden.mockRestore();
  });
});

describe("top poll", () => {
  it("defaults to every 5 s", () => {
    expect(TOP_POLL_MS).toBe(5000);
  });
});

describe("CLI hints", () => {
  it("renders the pool and metrics actions from the templates", () => {
    expect(setSitePoolMode.cli({ site: "loja.test", mode: "dedicated" })).toBe(
      "forge site pool loja.test dedicated",
    );
    expect(setSiteMetrics.cli({ site: "loja.test", enabled: false })).toBe(
      "forge site metrics loja.test off",
    );
    expect(setSiteMetrics.cli({ site: "loja.test", enabled: true })).toBe(
      "forge site metrics loja.test on",
    );
  });

  it("renders the stats commands", () => {
    expect(statsCli("loja.test")).toBe("forge stats loja.test");
    expect(statsCli("loja.test", "1h")).toBe("forge stats loja.test --window 1h");
    expect(topCli()).toBe("forge top");
  });
});
