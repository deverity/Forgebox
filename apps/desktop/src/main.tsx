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
 * The Tauri bridge in the app. Outside Tauri (vite dev/preview, tests, the qa mode) the scripted
 * fakeBridge stands in, picked by `?scenario=`; the production build drops that branch.
 */
async function createBridge(): Promise<Bridge> {
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

// Render even if i18n init fails: the UI must never stay blank because of it.
initI18n({ allowPseudo: import.meta.env.MODE !== "production" })
  .catch((error: unknown) => {
    console.warn("i18n init failed", error);
  })
  .then(createBridge)
  .then((bridge) => {
    render(container, bridge);
  })
  .catch((error: unknown) => {
    console.error("could not start the app", error);
  });
