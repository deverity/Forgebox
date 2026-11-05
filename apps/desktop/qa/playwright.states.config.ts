// State stills of the full Design QA review (M2-E4-T7): qa/states.spec.ts on the qa build, with
// the system Edge like the other suites (ADR-0036). Output in qa/out/states/<run>/ (gitignored),
// one contact sheet per screen through the qa:shots teardown; QA_SHOTS_RUN names the run.
import path from "node:path";
import { defineConfig } from "@playwright/test";

// A port of its own so this suite can run beside e2e (4174), qa:check (4175) and qa:shots (4176).
const PORT = Number(process.env.FORGE_QA_STATES_PORT ?? 4178);

process.env.QA_SHOTS_RUN ??= new Date().toISOString().replace(/[-:]/g, "").replace(/\..*/, "");
process.env.QA_SHOTS_DIR = path.resolve(
  import.meta.dirname,
  "out/states",
  process.env.QA_SHOTS_RUN,
);

export default defineConfig({
  testDir: ".",
  testMatch: "states.spec.ts",
  outputDir: "./out/states-results",
  globalTeardown: "./contact-sheet.ts",
  fullyParallel: true,
  workers: 3,
  forbidOnly: !!process.env.CI,
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
