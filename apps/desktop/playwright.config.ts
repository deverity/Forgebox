// Playwright against the qa-mode build served by `vite preview` (ADR-0036). Uses the system
// Edge (channel msedge): run with PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1, nothing is downloaded.
// Fallback when Edge is unusable: chromium headless shell with PLAYWRIGHT_BROWSERS_PATH
// pointing at a project-local folder (drop `channel`).
import { defineConfig } from "@playwright/test";

const PORT = 4174;

export default defineConfig({
  testDir: "./e2e",
  testIgnore: ["real/**"],
  outputDir: "./qa/out/test-results",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: [["list"]],
  timeout: 30_000,
  use: {
    baseURL: `http://127.0.0.1:${String(PORT)}`,
    channel: "msedge",
    headless: true,
    trace: "retain-on-failure",
  },
  webServer: {
    command: "pnpm build:qa && pnpm preview:qa",
    url: `http://127.0.0.1:${String(PORT)}`,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});
