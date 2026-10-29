// Shared Playwright helpers (ADR-0036): scenario URLs, emulation, axe, keyboard traversal, CLS
// and a console-error collector that fails the test when the page logs errors.
import AxeBuilder from "@axe-core/playwright";
import { test as base, expect, type CDPSession, type Page } from "@playwright/test";

export const SCENARIOS = [
  "empty",
  "populated",
  "heavy",
  "errors",
  "not-implemented",
  "daemon-down",
  "slow",
  "remote",
] as const;
export type Scenario = (typeof SCENARIOS)[number];

export interface QaUrl {
  scenario?: Scenario;
  theme?: "light" | "dark";
  vibrancy?: "on" | "off";
  motion?: "reduced" | "full";
  locale?: "pt-BR" | "en-US" | "pseudo";
  latency?: number;
}

export function qaUrl(options: QaUrl = {}): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(options)) params.set(key, String(value));
  const query = params.toString();
  return query === "" ? "/" : `/?${query}`;
}

export interface Emulation {
  deviceScaleFactor?: number;
  colorScheme?: "light" | "dark";
  reducedMotion?: boolean;
  /** CSS `prefers-reduced-transparency` (not covered by Playwright's own emulation). */
  reducedTransparency?: boolean;
}

/** Media features through CDP `Emulation.setEmulatedMedia` (+ scale factor via a new context). */
export async function emulate(page: Page, options: Emulation): Promise<CDPSession> {
  const session = await page.context().newCDPSession(page);
  const features: { name: string; value: string }[] = [];
  if (options.colorScheme) {
    features.push({ name: "prefers-color-scheme", value: options.colorScheme });
  }
  if (options.reducedMotion !== undefined) {
    features.push({
      name: "prefers-reduced-motion",
      value: options.reducedMotion ? "reduce" : "no-preference",
    });
  }
  if (options.reducedTransparency !== undefined) {
    features.push({
      name: "prefers-reduced-transparency",
      value: options.reducedTransparency ? "reduce" : "no-preference",
    });
  }
  await session.send("Emulation.setEmulatedMedia", { features });
  if (options.deviceScaleFactor !== undefined) {
    const viewport = page.viewportSize() ?? { width: 1280, height: 800 };
    await session.send("Emulation.setDeviceMetricsOverride", {
      ...viewport,
      deviceScaleFactor: options.deviceScaleFactor,
      mobile: false,
    });
  }
  return session;
}

/** axe-core violations of serious or critical impact. */
export async function axeViolations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page }).analyze();
  return result.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.help} (${String(v.nodes.length)} nodes)`);
}

/** Tabs through the page and returns a description of each focused element, in order. */
export async function tabOrder(page: Page, max = 40): Promise<string[]> {
  const seen: string[] = [];
  await page.evaluate(() => {
    (document.activeElement as HTMLElement | null)?.blur();
  });
  for (let i = 0; i < max; i += 1) {
    await page.keyboard.press("Tab");
    const label = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return "";
      const name = el.getAttribute("aria-label") ?? el.textContent.trim().slice(0, 40);
      return `${el.tagName.toLowerCase()}[${el.getAttribute("role") ?? ""}] ${name}`;
    });
    if (label === "" || seen.includes(label)) break;
    seen.push(label);
  }
  return seen;
}

/** Starts a layout-shift observer; `read()` returns the cumulative score so far. */
export async function trackCls(page: Page): Promise<() => Promise<number>> {
  await page.addInitScript(() => {
    const w = window as unknown as { __cls: number };
    w.__cls = 0;
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const shift = entry as PerformanceEntry & { value: number; hadRecentInput: boolean };
        if (!shift.hadRecentInput) w.__cls += shift.value;
      }
    }).observe({ type: "layout-shift", buffered: true });
  });
  return () => page.evaluate(() => (window as unknown as { __cls: number }).__cls);
}

interface Fixtures {
  /** Console errors and page errors collected while the test runs; asserted empty at the end. */
  consoleErrors: string[];
}

export const test = base.extend<Fixtures>({
  consoleErrors: [
    async ({ page }, use) => {
      const errors: string[] = [];
      page.on("console", (msg) => {
        if (msg.type() === "error") errors.push(msg.text());
      });
      page.on("pageerror", (error) => {
        errors.push(error.message);
      });
      await use(errors);
      expect(errors, "console errors").toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
