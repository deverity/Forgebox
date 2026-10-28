import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { initI18n } from "./i18n";
import "./index.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("#root not found in index.html");
}

// Core namespaces load before first paint (tiny JSON); the daemon connection comes later (T8).
void initI18n({ allowPseudo: import.meta.env.MODE !== "production" }).then(() => {
  createRoot(container).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
});
