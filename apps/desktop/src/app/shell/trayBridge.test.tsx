// M2-E1-T15: the tray opens routes in the window and learns prefers-reduced-motion.
import { act, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { mockReducedMotion } from "../../test/matchMedia";
import { renderApp } from "../testApp";
import { TRAY_ROUTES, trayHostOverride, trayRoute, type TrayHost } from "./trayBridge";

function fakeHost() {
  let handler: ((payload: unknown) => void) | null = null;
  const reported: boolean[] = [];
  const unlisten = vi.fn();
  const host: TrayHost = {
    onNavigate(h) {
      handler = h;
      return Promise.resolve(unlisten);
    },
    reportMotion(reduced) {
      reported.push(reduced);
      return Promise.resolve();
    },
  };
  return {
    host,
    reported,
    unlisten,
    emit(payload: unknown) {
      handler?.(payload);
    },
  };
}

afterEach(() => {
  trayHostOverride.host = null;
});

describe("tray routes", () => {
  it("accepts only the routes the tray opens", () => {
    for (const route of TRAY_ROUTES) expect(trayRoute(route)).toBe(route);
    for (const junk of ["/", "/terminal", "https://x", 3, null, { to: "/logs" }]) {
      expect(trayRoute(junk)).toBeNull();
    }
  });
});

describe("useTrayBridge", () => {
  it("navigates on the tray's request and reports reduced motion", async () => {
    mockReducedMotion(true);
    const fake = fakeHost();
    trayHostOverride.host = fake.host;
    const h = await renderApp({ path: "/dashboard" });
    await waitFor(() => {
      expect(fake.reported).toEqual([true]);
    });
    act(() => {
      fake.emit("/logs");
    });
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/logs");
    });
    act(() => {
      fake.emit("/not-a-tray-route");
      fake.emit("/settings");
    });
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/settings");
    });
  });
});
