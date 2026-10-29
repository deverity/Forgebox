import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { bootAppearance, followAppearance, isTauri } from "./app/appearance";
import { initI18n } from "./i18n";
import "./index.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("#root not found in index.html");
}

// data-theme / data-vibrancy before the first paint (the window is hidden until `ready`).
bootAppearance();
// Warm the IPC module so `ready` goes out right after the first frame.
if (isTauri()) void import("@tauri-apps/api/core");

// Core namespaces load before first paint (tiny JSON); the daemon connection comes later (T8).
function render(root: HTMLElement) {
  createRoot(root).render(
    <StrictMode>
      <App />
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
  .finally(() => {
    render(container);
  });
