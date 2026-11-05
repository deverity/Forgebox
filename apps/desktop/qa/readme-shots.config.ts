// `pnpm qa:readme-shots` (M2-E4-T12): captures the README screenshots from the qa build with the
// system Edge, like the other suites (ADR-0036). Output: <repo>/docs/assets/screenshots/*.png.
import { defineConfig } from "@playwright/test";

const PORT = Number(process.env.FORGE_QA_README_PORT ?? 4177);

export default defineConfig({
  testDir: ".",
  testMatch: "readme-shots.spec.ts",
  outputDir: "./out/readme-shots-results",
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  timeout: 60_000,
  use: {
    baseURL: `http://127.0.0.1:${String(PORT)}`,
    channel: "msedge",
    headless: true,
  },
  webServer: {
    command: `pnpm build:qa && pnpm preview:qa --port ${String(PORT)}`,
    url: `http://127.0.0.1:${String(PORT)}`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
