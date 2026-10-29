import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Bridge, ContextStateDto, StreamFrame } from "../ipc/bridge";
import { createFakeBridge } from "../ipc/fakeBridge";
import { manualFrameClock } from "./frame";
import { StateLayer } from "./layer";
import { manualPageActivity } from "./page";
import { ForgeStateProvider, useConnectionStatus, useSites } from "./react";
import { Draft, createContextStores, reconcile } from "./stores";
import {
  IDS,
  populatedData,
  site,
  startHarness,
  stateScenario,
  type Harness,
} from "./testFixtures";

let harness: Harness | null = null;
let layer: StateLayer | null = null;
afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  await layer?.dispose();
  harness = null;
  layer = null;
});

function Status() {
  const status = useConnectionStatus();
  const sites = useSites();
  return (
    <p data-testid="status">
      {status}:{Object.keys(sites).length}
    </p>
  );
}

describe("ContextRuntime", () => {
  it("paints before any daemon traffic, then connects and loads", async () => {
    const fake = createFakeBridge(stateScenario(populatedData()));
    const calls: string[] = [];
    const spy: Bridge = {
      state: (context) => {
        calls.push("state");
        return fake.state(context);
      },
      call: (request) => fake.call(request),
      subscribe: (context, method, params, onFrame) =>
        fake.subscribe(context, method, params, onFrame),
      start: (context, options) => fake.start(context, options),
      onConnection: (listener) => {
        calls.push("onConnection");
        return fake.onConnection(listener);
      },
    };
    layer = new StateLayer(spy, {
      page: manualPageActivity(),
      frame: manualFrameClock(),
      rttIntervalMs: 0,
    });
    let paintedBeforeTraffic = false;
    function Probe() {
      paintedBeforeTraffic = calls.length === 0;
      return null;
    }
    render(
      <ForgeStateProvider layer={layer}>
        <Probe />
        <Status />
      </ForgeStateProvider>,
    );
    expect(paintedBeforeTraffic).toBe(true);
    await waitFor(() => {
      expect(screen.getByTestId("status").textContent).toBe("connected:1");
    });
  });

  it("follows the connection and keeps only newer context states", async () => {
    harness = await startHarness();
    const { connection } = harness.runtime.stores;
    expect(connection.getState().status).toBe("connected");
    act(() => {
      harness?.fake.crash();
    });
    expect(connection.getState()).toMatchObject({ status: "reconnecting", hello: null });
    const current = connection.getState().state;
    if (current === null) throw new Error("no context state");
    const stale: ContextStateDto = { ...current, status: "connected", version: 0 };
    // An older version never overwrites a newer one.
    (harness.runtime as unknown as { onState(s: ContextStateDto): void }).onState(stale);
    expect(connection.getState().status).toBe("reconnecting");
    act(() => {
      harness?.fake.autoRestart();
    });
    expect(connection.getState().status).toBe("connected");
  });

  it("measures the round trip while connected and visible", async () => {
    let now = 0;
    harness = await startHarness({
      rttIntervalMs: 10_000,
      now: () => now,
      scenario: {
        handlers: {
          "system.ping": () => {
            now += 7;
            return { server_time: "2026-10-28T12:00:00Z" };
          },
        },
      },
    });
    const h = harness;
    await waitFor(() => {
      expect(h.runtime.stores.connection.getState().rttMs).toBe(7);
    });
  });

  it("reopens the event stream from its cursor when it ends", async () => {
    let frames: ((frame: StreamFrame) => void) | null = null;
    const fake = createFakeBridge(stateScenario(populatedData()));
    const subscribes: unknown[] = [];
    const bridge: Bridge = {
      state: (c) => fake.state(c),
      call: (r) => fake.call(r),
      start: (c, o) => fake.start(c, o),
      onConnection: (l) => fake.onConnection(l),
      subscribe: async (context, method, params, onFrame) => {
        subscribes.push(params);
        frames = onFrame;
        return fake.subscribe(context, method, params, onFrame);
      },
    };
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    layer = new StateLayer(bridge, {
      page: manualPageActivity(),
      frame: manualFrameClock(),
      rttIntervalMs: 0,
      snapshotDebounceMs: 0,
    });
    const runtime = layer.runtime("local");
    await runtime.start();
    await runtime.settled();
    fake.publish("actor.restarted", {
      v: 1,
      actor: "a",
      reason: "r",
      restart_count: 1,
      backoff_ms: 1,
    });
    fake.flush();
    expect(runtime.stores.sync.getState().cursor).toBe(1);
    (frames as unknown as (frame: StreamFrame) => void)({
      stream_id: "s",
      frame: 99,
      entries: [{ kind: "end", error: { code: "internal", message: "gone" } }],
    });
    await waitFor(() => {
      expect(subscribes).toHaveLength(2);
    });
    expect(subscribes[1]).toEqual({ resume_from: 1 });
    warn.mockRestore();
  });
});

describe("Draft and reconcile", () => {
  it("copies on write and commits once", () => {
    const stores = createContextStores("local");
    const listener = vi.fn();
    stores.sites.subscribe(listener);
    const draft = new Draft(stores.sites);
    const a = site(IDS.site, "loja.test");
    draft.put("sites", a.id, a);
    draft.put("sites", a.id, a);
    draft.remove("sites", "missing");
    expect(stores.sites.getState().sites).toEqual({});
    expect(draft.commit()).toBe(true);
    expect(draft.commit()).toBe(false);
    expect(listener).toHaveBeenCalledOnce();
    expect(stores.sites.getState().sites[a.id]).toBe(a);
  });

  it("reuses unchanged objects and the map itself when nothing moved", () => {
    const a = site(IDS.site, "loja.test");
    const b = site(IDS.site2, "blog.test");
    const previous = { [a.id]: a, [b.id]: b };
    expect(reconcile(previous, [{ ...a }, { ...b }], (s) => s.id)).toBe(previous);
    const next = reconcile(previous, [{ ...a }, { ...b, secure: false }], (s) => s.id);
    expect(next).not.toBe(previous);
    expect(next[a.id]).toBe(a);
    expect(next[b.id]).not.toBe(b);
  });
});
