// Playwright against the qa-mode build served by `vite preview` (ADR-0036). Uses the system
// Edge (channel msedge): run with PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1, nothing is downloaded.
// Fallback when Edge is unusable: chromium headless shell with PLAYWRIGHT_BROWSERS_PATH
// pointing at a project-local folder (drop `channel`).
import { defineConfig } from "@playwright/test";

// FORGE_QA_PORT lets several worktrees run side by side; the server is never reused, so a stale
// dist-qa or another worktree's preview cannot answer for this code.
const PORT = Number(process.env.FORGE_QA_PORT ?? 4174);

export default defineConfig({
  testDir: "./e2e",
  // real/ runs only through scripts/e2e-desktop.ps1 and live/ through scripts/gui-e2e.ps1 (its
  // *.test.ts files are vitest); neither belongs to the fake-bridge suite.
  testIgnore: ["real/**", "live/**"],
  outputDir: "./qa/out/test-results",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  // CI also writes an HTML report that the workflow uploads together with test-results.
  reporter: process.env.CI
    ? [["list"], ["html", { outputFolder: "./qa/out/playwright-report", open: "never" }]]
    : [["list"]],
  timeout: 30_000,
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
