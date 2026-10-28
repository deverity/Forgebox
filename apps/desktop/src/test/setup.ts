import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, beforeAll } from "vitest";
import { initI18n } from "../i18n";

// Tests run in en-US so assertions are stable regardless of the machine locale.
beforeAll(async () => {
  await initI18n({ override: "en-US", setLanguage: () => Promise.resolve() });
});

// Vitest globals are off, so Testing Library cannot register its own cleanup.
afterEach(cleanup);
