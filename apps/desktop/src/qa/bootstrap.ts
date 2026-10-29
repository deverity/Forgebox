// QA-mode bootstrap (ADR-0036). Reached only from `import.meta.env.MODE === "qa"` in main.tsx,
// so production builds drop this module (a sentinel check scans dist: e2e/dist.test.ts).
//
// URL parameters (all optional):
//   scenario   empty | populated | heavy | errors | not-implemented | daemon-down | slow | remote
//   theme      light | dark              (data-theme)
//   vibrancy   on | off                 (data-vibrancy)
//   motion     reduced | full           (reduced = prefers-reduced-motion matches)
//   locale     pt-BR | en-US | pseudo
//   latency    ms added to every call of the scenario
// The fake bridge is published on `window.__FORGEBOX_QA__` for Playwright (`page.evaluate`).
import { registerQaScenarios, liveBehaviourOf } from "../../qa/fixtures/scenarios";
import { applyAppearance } from "../app/appearance";
import { createFakeBridge, scenarioFromSearch, type FakeBridge } from "../ipc/fakeBridge";
import type { AnyLocale } from "../i18n";
import { qaLocaleOverride } from "./pseudo-locale";

/** Unique string that must never appear in a production bundle. */
export const QA_SENTINEL = "FORGEBOX_QA_SENTINEL_9c1e5b";

export interface QaHandle {
  sentinel: string;
  scenario: string;
  bridge: FakeBridge;
  locale: AnyLocale | null;
  stop: () => void;
}

declare global {
  interface Window {
    __FORGEBOX_QA__?: QaHandle;
  }
}

const REDUCED_QUERY = "(prefers-reduced-motion: reduce)";

function forceReducedMotion(win: Window): void {
  const original = win.matchMedia.bind(win);
  win.matchMedia = (query: string) => {
    const real = original(query);
    if (query.replace(/\s+/g, " ").trim() !== REDUCED_QUERY) return real;
    return new Proxy(real, {
      get(target, prop) {
        if (prop === "matches") return true;
        const value = Reflect.get(target, prop, target) as unknown;
        return typeof value === "function" ? (value as () => unknown).bind(target) : value;
      },
    });
  };
}

/** Applies the URL's appearance/motion/locale and creates the scenario's fake bridge. */
export function bootQa(search: string = window.location.search, win: Window = window): QaHandle {
  const params = new URLSearchParams(search);
  registerQaScenarios();

  const theme = params.get("theme");
  const vibrancy = params.get("vibrancy");
  applyAppearance({
    ...(theme === "light" || theme === "dark" ? { theme } : {}),
    ...(vibrancy === "on" || vibrancy === "off" ? { vibrancy } : {}),
  });
  if (params.get("motion") === "reduced") forceReducedMotion(win);

  const name = params.get("scenario") ?? "empty";
  const scenario = scenarioFromSearch(search);
  const latency = Number(params.get("latency"));
  if (params.has("latency") && Number.isFinite(latency) && latency >= 0) {
    scenario.latencyMs = latency;
  }
  const bridge = createFakeBridge(scenario);
  const { live, data } = liveBehaviourOf(name);
  const stopLive = live?.(bridge, data);

  const handle: QaHandle = {
    sentinel: QA_SENTINEL,
    scenario: scenario.name ?? name,
    bridge,
    locale: qaLocaleOverride(params.get("locale")),
    stop: () => {
      stopLive?.();
    },
  };
  win.__FORGEBOX_QA__ = handle;
  return handle;
}
