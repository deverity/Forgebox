import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { bootAppearance, followAppearance, isTauri } from "./app/appearance";
import { initI18n } from "./i18n";
import type { Bridge } from "./ipc/bridge";
import { createTauriBridge } from "./ipc/tauriBridge";
import { StateLayer } from "./state";
import "./index.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("#root not found in index.html");
}

// data-theme / data-vibrancy before the first paint (the window is hidden until `ready`).
bootAppearance();
// Warm the IPC module so `ready` goes out right after the first frame.
if (isTauri()) void import("@tauri-apps/api/core");

/**
 * One bridge-selection path: the qa-mode scenario bridge (bootQa, `window.__FORGEBOX_QA__`),
 * else the Tauri bridge, else (plain dev outside Tauri) the scripted fakeBridge picked by
 * `?scenario=`. The production build drops the fake branch.
 */
async function pickBridge(qaBridge: Bridge | undefined): Promise<Bridge> {
  if (qaBridge !== undefined) return qaBridge;
  if (!isTauri() && import.meta.env.MODE !== "production") {
    const { createFakeBridge, scenarioFromSearch } = await import("./ipc/fakeBridge");
    return createFakeBridge(scenarioFromSearch(window.location.search));
  }
  return createTauriBridge();
}

// Core namespaces load before first paint (tiny JSON); the daemon connection starts after the
// first commit (ForgeStateProvider), so it never blocks the paint.
function render(root: HTMLElement, bridge: Bridge) {
  createRoot(root).render(
    <StrictMode>
      <App layer={new StateLayer(bridge)} />
    </StrictMode>,
  );
  // Live theme/material changes; never on the first-paint path.
  void followAppearance();
}

// QA builds (`vite build --mode qa`, ADR-0036) install the scenario-driven fake bridge first.
// `MODE` is a build-time constant: production builds drop the branch and the whole src/qa tree.
const qa =
  import.meta.env.MODE === "qa"
    ? import("./qa/bootstrap").then((m) => m.bootQa())
    : Promise.resolve(null);

// Render even if i18n init fails: the UI must never stay blank because of it.
qa.then((handle) =>
  initI18n({
    allowPseudo: import.meta.env.MODE !== "production",
    ...(handle?.locale ? { override: handle.locale } : {}),
  }),
)
  .catch((error: unknown) => {
    console.warn("i18n init failed", error);
  })
  .then(async () => pickBridge((await qa)?.bridge))
  .then((bridge) => {
    render(container, bridge);
  })
  .catch((error: unknown) => {
    console.error("could not start the app", error);
  });
