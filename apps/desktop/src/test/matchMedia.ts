// Controllable window.matchMedia for prefers-reduced-motion tests.
import { afterEach } from "vitest";

// jsdom has no matchMedia, so there is normally nothing to restore.
const original: unknown = Reflect.get(window, "matchMedia");

export function mockReducedMotion(reduced: boolean): void {
  const noop = () => undefined;
  Reflect.set(window, "matchMedia", (query: string) => ({
    matches: reduced && query.includes("prefers-reduced-motion"),
    media: query,
    onchange: null,
    addEventListener: noop,
    removeEventListener: noop,
    dispatchEvent: () => false,
  }));
}

afterEach(() => {
  if (original === undefined) Reflect.deleteProperty(window, "matchMedia");
  else Reflect.set(window, "matchMedia", original);
});
