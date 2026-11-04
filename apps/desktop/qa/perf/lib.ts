// Shared pieces of the qa:perf specs (M2-E4-T4): metric recording with thresholds, the JSON
// output, and the in-page probes. A metric listed in qa/perf/informative.json (variation above
// 15 % over three local runs, see scripts/qa-perf.mjs) is recorded but does not fail the spec.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, type Page } from "@playwright/test";
import { FEATURES } from "../../src/app/features";

export { expect, test } from "../../e2e/fixtures";

const OUT = join("qa", "out", "perf");

export interface Metric {
  name: string;
  value: number;
  unit: string;
  /** `max`: the value must stay at or below the threshold; `min`: at or above. */
  bound: "max" | "min";
  threshold: number;
  /** Over the threshold but not failing: the metric is too noisy on this machine. */
  informative: boolean;
  pass: boolean;
}

function informativeList(): string[] {
  try {
    return JSON.parse(readFileSync(join("qa", "perf", "informative.json"), "utf8")) as string[];
  } catch {
    return [];
  }
}

/** Collects the metrics of one spec file, writes `<spec>.json` and fails above a threshold. */
export class PerfReport {
  private readonly metrics: Metric[] = [];
  private readonly notes: Record<string, unknown> = {};
  private readonly spec: string;

  constructor(spec: string) {
    this.spec = spec;
  }

  record(
    name: string,
    value: number,
    unit: string,
    bound: Metric["bound"],
    threshold: number,
  ): Metric {
    const informative = informativeList().includes(`${this.spec}.${name}`);
    const pass = bound === "max" ? value <= threshold : value >= threshold;
    const metric: Metric = { name, value, unit, bound, threshold, informative, pass };
    this.metrics.push(metric);
    return metric;
  }

  note(key: string, value: unknown): void {
    this.notes[key] = value;
  }

  /** Writes the JSON (even when a metric failed) and asserts every non-informative metric. */
  finish(): void {
    const dir = process.env.FORGE_PERF_OUT ?? OUT;
    mkdirSync(dir, { recursive: true });
    writeFileSync(
      join(dir, `${this.spec}.json`),
      `${JSON.stringify({ spec: this.spec, metrics: this.metrics, notes: this.notes }, null, 2)}\n`,
    );
    for (const m of this.metrics) {
      if (m.informative) continue;
      const word = m.bound === "max" ? "at most" : "at least";
      expect(
        m.pass,
        `${this.spec}.${m.name} = ${String(m.value)} ${m.unit}, expected ${word} ${String(m.threshold)}`,
      ).toBe(true);
    }
  }
}

export function perfUrl(path: string, scenario: string, extra: Record<string, string> = {}) {
  const params = new URLSearchParams({ scenario, live: "off", motion: "full", ...extra });
  return `${path}?${params.toString()}`;
}

/** Loads a route and waits until the shell is up and the qa handle exists. */
export async function openApp(page: Page, url: string): Promise<void> {
  await page.goto(url);
  await page.waitForSelector('[data-testid="app-shell"]');
  await page.waitForFunction(() => window.__FORGEBOX_QA__ !== undefined);
}

/** Sidebar routes (the `main` group and the footer), as `{id, path}`. */
export const NAV_ROUTES = FEATURES.filter((f) => f.nav !== "none").map((f) => ({
  id: f.id,
  path: f.path,
}));

export interface PerfProbe {
  longTasks: number[];
  events: number[];
}

declare global {
  interface Window {
    __perf?: PerfProbe;
  }
}

/** Starts the long-task and event-timing observers (call after the page settled). */
export async function startProbe(page: Page): Promise<void> {
  await page.evaluate(() => {
    const probe: PerfProbe = { longTasks: [], events: [] };
    window.__perf = probe;
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) probe.longTasks.push(entry.duration);
    }).observe({ type: "longtask" });
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (["click", "pointerdown", "pointerup", "keydown"].includes(entry.name)) {
          probe.events.push(entry.duration);
        }
      }
    }).observe({ type: "event", durationThreshold: 16 } as PerformanceObserverInit);
  });
}

/** Lets pending observer callbacks run, then returns what the probe saw. */
export async function readProbe(page: Page): Promise<PerfProbe> {
  await page.evaluate(() => new Promise((done) => setTimeout(done, 150)));
  return page.evaluate(() => window.__perf ?? { longTasks: [], events: [] });
}

export const maxOf = (values: readonly number[]): number =>
  values.length === 0 ? 0 : Math.max(...values);

export function medianOf(values: readonly number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length === 0) return 0;
  return sorted.length % 2 === 1
    ? (sorted[mid] ?? 0)
    : ((sorted[mid - 1] ?? 0) + (sorted[mid] ?? 0)) / 2;
}

/**
 * Clicks the sidebar link of `path` inside the page and returns the milliseconds until the
 * route's page is mounted (`data-testid="page-<id>"`) and one frame was painted after it.
 */
export async function navigateTo(page: Page, id: string, path: string): Promise<number> {
  return page.evaluate(
    ({ id: target, path: href }) =>
      new Promise<number>((resolve, reject) => {
        const link = document.querySelector<HTMLAnchorElement>(
          `[data-testid="sidebar"] a[href="${href}"]`,
        );
        if (link === null) {
          reject(new Error(`no sidebar link for ${href}`));
          return;
        }
        const started = performance.now();
        const poll = () => {
          if (
            window.location.pathname === href &&
            document.querySelector(`[data-testid="page-${target}"]`) !== null
          ) {
            requestAnimationFrame(() => {
              resolve(performance.now() - started);
            });
            return;
          }
          if (performance.now() - started > 5000) {
            reject(new Error(`navigation to ${href} did not finish`));
            return;
          }
          requestAnimationFrame(poll);
        };
        link.click();
        poll();
      }),
    { id, path },
  );
}
