// `pnpm qa:check` (M2-E4-T3): the docs/07 section 11 checklist over the fakeBridge scenarios, on
// the same qa build and Edge as the e2e suite (ADR-0036). JSON + HTML reports land in
// qa/out/check-report (gitignored); CI uploads them.
import { defineConfig } from "@playwright/test";

// A different default port than playwright.config.ts so both suites can run side by side.
const PORT = Number(process.env.FORGE_QA_CHECK_PORT ?? 4175);

export default defineConfig({
  testDir: "./qa/checks",
  outputDir: "./qa/out/check-results",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: [
    ["list"],
    ["json", { outputFile: "./qa/out/check-report/report.json" }],
    ["html", { outputFolder: "./qa/out/check-report/html", open: "never" }],
  ],
  timeout: 45_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: `http://127.0.0.1:${String(PORT)}`,
    channel: "msedge",
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: `pnpm build:qa && pnpm preview:qa --port ${String(PORT)}`,
    url: `http://127.0.0.1:${String(PORT)}`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
