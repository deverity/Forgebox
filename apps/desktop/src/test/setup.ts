import "@testing-library/jest-dom/vitest";
import { cleanup, configure } from "@testing-library/react";
import { afterEach, beforeAll } from "vitest";
import { initI18n } from "../i18n";

// Tests run in en-US so assertions are stable regardless of the machine locale.
beforeAll(async () => {
  await initI18n({ override: "en-US", setLanguage: () => Promise.resolve() });
});

// Lazy route chunks load on first use; a busy machine (parallel builds) needs more than 1 s.
configure({ asyncUtilTimeout: 5000 });

// Vitest globals are off, so Testing Library cannot register its own cleanup.
afterEach(cleanup);

// jsdom lacks these browser APIs, which Radix (Select, Slider) relies on.
/* eslint-disable @typescript-eslint/no-empty-function, @typescript-eslint/no-unnecessary-condition, @typescript-eslint/unbound-method */
if (typeof Element !== "undefined") {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver ??= ResizeObserverStub;
  Element.prototype.hasPointerCapture ??= () => false;
  Element.prototype.setPointerCapture ??= () => {};
  Element.prototype.releasePointerCapture ??= () => {};
  Element.prototype.scrollIntoView ??= () => {};
}
/* eslint-enable @typescript-eslint/no-empty-function, @typescript-eslint/no-unnecessary-condition, @typescript-eslint/unbound-method */
