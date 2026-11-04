// `pnpm qa:perf` (M2-E4-T4): the docs/07 section 10 budgets over the fakeBridge scenarios, on the
// same qa build and Edge as the e2e suite (ADR-0036). One worker and no parallelism: the numbers
// are timings. JSON results land in qa/out/perf (gitignored); scripts/qa-perf.mjs runs the suite
// three times and reports each metric's coefficient of variation.
import { defineConfig } from "@playwright/test";

// Its own default port so it can run next to the e2e (4174) and qa:check (4175) suites.
const PORT = Number(process.env.FORGE_QA_PERF_PORT ?? 4176);

export default defineConfig({
  testDir: "./qa/perf",
  testMatch: "*.spec.ts",
  outputDir: "./qa/out/perf-results",
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  reporter: [["list"]],
  timeout: 60_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: `http://127.0.0.1:${String(PORT)}`,
    channel: "msedge",
    headless: true,
    viewport: { width: 1280, height: 800 },
    trace: "retain-on-failure",
  },
  webServer: {
    command: `pnpm build:qa && pnpm preview:qa --port ${String(PORT)}`,
    url: `http://127.0.0.1:${String(PORT)}`,
    reuseExistingServer: process.env.FORGE_PERF_REUSE === "1",
    timeout: 180_000,
  },
});
