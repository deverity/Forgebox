import { QueryClient } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Invalidator, queryKeys } from "./queries";
import { ForgeStateProvider, useForgeQuery } from "./react";
import { IDS, site, startHarness, type Harness } from "./testFixtures";

let harness: Harness | null = null;
afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  harness = null;
});

describe("Invalidator", () => {
  it("invalidates each stale method once per debounce window", () => {
    vi.useFakeTimers();
    const client = new QueryClient();
    const spy = vi.spyOn(client, "invalidateQueries");
    const invalidator = new Invalidator(client, "local", 200);
    for (let i = 0; i < 100; i += 1) invalidator.onEvent("site.updated");
    invalidator.onEvent("service.metrics"); // invalidates nothing
    invalidator.onEvent("future.kind");
    expect(spy).not.toHaveBeenCalled();
    vi.advanceTimersByTime(200);
    const keys = spy.mock.calls.map(([filters]) => filters?.queryKey);
    expect(keys).toEqual([
      ["local", "sites.list"],
      ["local", "sites.info"],
      ["local", "sites.parked"],
      ["local", "sites.ps"],
    ]);
    invalidator.dispose();
  });

  it("invalidates the whole context after a resync", () => {
    const client = new QueryClient();
    const spy = vi.spyOn(client, "invalidateQueries");
    new Invalidator(client, "local").all();
    expect(spy).toHaveBeenCalledWith({ queryKey: queryKeys.context("local") });
  });
});

describe("useForgeQuery", () => {
  it("caches per (context, method, params) and refetches after the events that make it stale", async () => {
    harness = await startHarness();
    const h = harness;
    const wrapper = ({ children }: { children: ReactNode }) => (
      <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
    );
    const { result } = renderHook(() => useForgeQuery("sites.list", {}), { wrapper });
    await waitFor(() => {
      expect(result.current.data?.sites.map((s) => s.host)).toEqual(["loja.test"]);
    });
    expect(
      h.layer.queryClient.getQueryData(queryKeys.call("local", "sites.list", {})),
    ).toBeDefined();
    h.data.sites.push(site(IDS.site2, "blog.test"));
    h.fake.publish("site.created", { v: 1, site_id: IDS.site2, host: "blog.test" });
    await waitFor(() => {
      expect(result.current.data?.sites.map((s) => s.host)).toEqual(["loja.test", "blog.test"]);
    });
  });

  it("does not call methods the daemon does not implement", async () => {
    harness = await startHarness({ scenario: { notImplemented: ["mail.status"] } });
    const h = harness;
    const wrapper = ({ children }: { children: ReactNode }) => (
      <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
    );
    const { result } = renderHook(() => useForgeQuery("mail.status", {}), { wrapper });
    expect(result.current.fetchStatus).toBe("idle");
    expect(h.count("mail.status")).toBe(0);
  });
});
