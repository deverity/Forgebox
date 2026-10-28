import { afterEach, vi } from "vitest";

/** Stubs matchMedia so `prefers-reduced-motion: reduce` matches (or not) for the next render. */
export function mockReducedMotion(reduced: boolean): void {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: reduced && query.includes("prefers-reduced-motion"),
    media: query,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    addListener: () => undefined,
    removeListener: () => undefined,
    onchange: null,
    dispatchEvent: () => false,
  }));
}

/** Call once at the top of a test file: undoes mockReducedMotion after every test. */
export function restoreMotion(): void {
  afterEach(() => {
    vi.unstubAllGlobals();
  });
}
