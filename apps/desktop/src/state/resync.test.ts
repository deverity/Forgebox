// Acceptance (M2-E1-T9 #2): `reset`, `lagged` or a reconnection to a daemon whose
// `last_event_seq` is below our cursor refetch the snapshots exactly once (debounced).
import { act } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SLICES, SNAPSHOTS } from "./snapshots";
import { IDS, site, startHarness, type Harness } from "./testFixtures";

const SNAPSHOT_METHODS = SLICES.map((slice) => SNAPSHOTS[slice].method);

let harness: Harness | null = null;
afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

function snapshotCalls(h: Harness): Record<string, number> {
  return Object.fromEntries(SNAPSHOT_METHODS.map((method) => [method, h.count(method)]));
}

function plusOne(before: Record<string, number>): Record<string, number> {
  return Object.fromEntries(Object.entries(before).map(([method, n]) => [method, n + 1]));
}

/** Delivers the fake's frames, applies them and waits for the debounced snapshots. */
async function settle(h: Harness): Promise<void> {
  await act(async () => {
    h.fake.flush();
    await new Promise((resolve) => setTimeout(resolve, 20));
    h.runtime.flushEvents();
    await h.runtime.settled();
  });
}

describe("resync", () => {
  it("loads every snapshot once at start", async () => {
    harness = await startHarness();
    expect(snapshotCalls(harness)).toEqual(
      Object.fromEntries(SNAPSHOT_METHODS.map((method) => [method, 1])),
    );
    const { services, sites, system, sync } = harness.runtime.stores;
    expect(Object.keys(services.getState().instances)).toEqual([IDS.mysql, IDS.redis]);
    expect(Object.keys(sites.getState().sites)).toEqual([IDS.site]);
    expect(system.getState().daemon?.version).toBe("0.2.0");
    expect(sync.getState().resyncs).toBe(1);
  });

  it("reset → snapshots refetched exactly once", async () => {
    harness = await startHarness();
    harness.fake.publish("actor.restarted", {
      v: 1,
      actor: "x",
      reason: "y",
      restart_count: 1,
      backoff_ms: 1,
    });
    await settle(harness);
    const before = snapshotCalls(harness);
    harness.fake.resetStreams();
    await settle(harness);
    expect(snapshotCalls(harness)).toEqual(plusOne(before));
    expect(harness.runtime.stores.sync.getState()).toMatchObject({
      cursor: 0,
      lastResyncReason: "reset",
    });
  });

  it("lagged → snapshots refetched exactly once", async () => {
    harness = await startHarness({ scenario: { historySize: 5 } });
    const before = snapshotCalls(harness);
    // Down while 20 events happen: the daemon only kept 5, so 15 were missed.
    harness.fake.disconnect();
    for (let i = 0; i < 20; i += 1) {
      harness.fake.publish("actor.restarted", {
        v: 1,
        actor: "x",
        reason: "y",
        restart_count: i,
        backoff_ms: 1,
      });
    }
    harness.fake.reconnect();
    await settle(harness);
    expect(snapshotCalls(harness)).toEqual(plusOne(before));
    expect(harness.runtime.stores.sync.getState().lastResyncReason).toBe("lagged");
  });

  it("a reconnection to a daemon behind our cursor → snapshots refetched exactly once", async () => {
    harness = await startHarness();
    for (let i = 0; i < 3; i += 1) {
      harness.fake.publish("actor.restarted", {
        v: 1,
        actor: "x",
        reason: "y",
        restart_count: i,
        backoff_ms: 1,
      });
    }
    await settle(harness);
    expect(harness.runtime.stores.sync.getState().cursor).toBe(3);
    const before = snapshotCalls(harness);
    harness.fake.disconnect();
    harness.fake.setHello({ last_event_seq: 1 });
    harness.fake.reconnect();
    await settle(harness);
    expect(snapshotCalls(harness)).toEqual(plusOne(before));
    expect(harness.runtime.stores.sync.getState()).toMatchObject({
      cursor: 1,
      lastResyncReason: "stale_seq",
    });
  });

  it("a burst of reset + lagged + stale seq → still exactly once", async () => {
    harness = await startHarness({ scenario: { historySize: 2 } });
    for (let i = 0; i < 4; i += 1) {
      harness.fake.publish("actor.restarted", {
        v: 1,
        actor: "x",
        reason: "y",
        restart_count: i,
        backoff_ms: 1,
      });
    }
    await settle(harness);
    const before = snapshotCalls(harness);
    harness.fake.disconnect();
    for (let i = 0; i < 6; i += 1) {
      harness.fake.publish("actor.restarted", {
        v: 1,
        actor: "x",
        reason: "y",
        restart_count: i,
        backoff_ms: 1,
      });
    }
    harness.fake.setHello({ last_event_seq: 0 });
    harness.fake.reconnect(); // lagged (history lost) + stale last_event_seq
    harness.fake.resetStreams(); // and the sequence restarts
    await settle(harness);
    expect(snapshotCalls(harness)).toEqual(plusOne(before));
  });

  it("a reconnection without a gap refetches nothing", async () => {
    harness = await startHarness();
    const before = snapshotCalls(harness);
    harness.fake.disconnect();
    harness.fake.publish("actor.restarted", {
      v: 1,
      actor: "x",
      reason: "y",
      restart_count: 1,
      backoff_ms: 1,
    });
    harness.fake.reconnect();
    await settle(harness);
    expect(snapshotCalls(harness)).toEqual(before);
  });

  it("refetches only the slice a payload cannot build (site.created → sites.list)", async () => {
    harness = await startHarness();
    const before = snapshotCalls(harness);
    harness.data.sites.push(site(IDS.site2, "blog.test"));
    for (let i = 0; i < 50; i += 1) {
      harness.fake.publish("site.created", { v: 1, site_id: IDS.site2, host: "blog.test" });
    }
    await settle(harness);
    expect(snapshotCalls(harness)).toEqual({
      ...before,
      "sites.list": (before["sites.list"] ?? 0) + 1,
    });
    expect(Object.keys(harness.runtime.stores.sites.getState().sites)).toEqual([
      IDS.site,
      IDS.site2,
    ]);
  });

  it("skips snapshot methods the daemon does not implement", async () => {
    harness = await startHarness({
      scenario: { notImplemented: ["tls.ca_status"] },
    });
    expect(harness.count("tls.ca_status")).toBe(0);
    expect(harness.count("services.status")).toBe(1);
  });

  it("keeps the previous objects when a refetch brings the same data", async () => {
    harness = await startHarness();
    const mysql = harness.runtime.stores.services.getState().instances[IDS.mysql];
    harness.runtime.resync("test");
    await settle(harness);
    expect(harness.runtime.stores.services.getState().instances[IDS.mysql]).toBe(mysql);
  });
});
