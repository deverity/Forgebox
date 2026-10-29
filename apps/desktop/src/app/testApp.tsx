// Test harness for the app shell (imported only by `*.test.tsx`): the whole <App> over a fake
// bridge with the state-layer snapshots answered, a memory-history router and a chosen window
// width (the sidebar collapses below 1100 px).
import { act, render, waitFor } from "@testing-library/react";
import { expect } from "vitest";
import { App } from "../App";
import { SCENARIOS, createFakeBridge, type FakeBridge, type FakeScenario } from "../ipc/fakeBridge";
import { manualPageActivity } from "../state/page";
import { StateLayer } from "../state/layer";
import {
  populatedData,
  stateScenario,
  timerFrameClock,
  type ScenarioData,
} from "../state/testFixtures";
import { createTestRouter, type AppRouter } from "./router";

export interface AppHarness {
  fake: FakeBridge;
  layer: StateLayer;
  router: AppRouter;
  data: ScenarioData;
  /** Lets the connection, the snapshots and a few frames settle. */
  settle(): Promise<void>;
  /** Resizes the window (fires `resize`). */
  resize(width: number): void;
}

export interface RenderAppOptions {
  path?: string;
  width?: number;
  /** A named scenario of `SCENARIOS` (e.g. `daemon-down`) merged over the snapshot handlers. */
  scenario?: string;
  extra?: FakeScenario;
  data?: ScenarioData;
}

let warmed: Promise<unknown> | undefined;

/**
 * Loads every lazy route module and shell chunk once per test file, so the first `renderApp()`
 * does not pay for cold module transforms inside its `findBy` window (cold-start flakiness).
 */
export function warmAppModules(): Promise<unknown> {
  warmed ??= Promise.all([
    import("../features/catalog"),
    import("../features/dashboard"),
    import("../features/databases"),
    import("../features/diagnostics"),
    import("../features/logs"),
    import("../features/mail"),
    import("../features/onboarding"),
    import("../features/php"),
    import("../features/services"),
    import("../features/settings"),
    import("../features/sites"),
    import("../features/terminal"),
    import("./gallery/Gallery"),
    import("./shell/ContextSelect"),
    import("../design/primitives/Toast"),
  ]);
  return warmed;
}

export async function renderApp(options: RenderAppOptions = {}): Promise<AppHarness> {
  await warmAppModules();
  window.innerWidth = options.width ?? 1280;
  const data = options.data ?? populatedData();
  const named = options.scenario === undefined ? {} : (SCENARIOS.get(options.scenario)?.() ?? {});
  const fake = createFakeBridge(stateScenario(data, { ...named, ...options.extra }));
  const layer = new StateLayer(fake, {
    page: manualPageActivity(),
    frame: timerFrameClock(),
    rttIntervalMs: 0,
    snapshotDebounceMs: 0,
    invalidateDebounceMs: 0,
  });
  const router = createTestRouter(options.path ?? "/dashboard");
  const settle = async () => {
    await act(async () => {
      await layer.runtime("local").settled();
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    // On a loaded machine the router, the event stream and the snapshots arrive later.
    await waitFor(() => {
      expect(router.state.status).toBe("idle");
      expect(router.state.resolvedLocation).toBeDefined();
    });
    const connection = layer.runtime("local").stores.connection;
    await waitFor(() => {
      expect(connection.getState().status).not.toBe("idle");
    });
    if (connection.getState().status === "connected") {
      await waitFor(() => {
        expect(fake.openStreams()).toBeGreaterThan(0);
        expect(layer.runtime("local").stores.services.getState().loaded).toBe(true);
      });
    }
  };
  render(<App layer={layer} router={router} />);
  await settle();
  return {
    fake,
    layer,
    router,
    data,
    settle,
    resize: (width) => {
      act(() => {
        window.innerWidth = width;
        window.dispatchEvent(new Event("resize"));
      });
    },
  };
}
