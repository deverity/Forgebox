// `pnpm qa:shots [--grep <text>]` (M2-E4-T2, docs/07 section 12): the screenshot matrix on the
// qa build, with the system Edge like the other suites (ADR-0036). Output goes to
// qa/out/shots/<run>/ (gitignored); QA_SHOTS_RUN names the run (default: a timestamp),
// QA_SHOTS_SUBSET=ci runs the reduced subset PRs use.
import path from "node:path";
import { defineConfig } from "@playwright/test";

// A port of its own so this suite can run beside e2e (4174) and qa:check (4175).
const PORT = Number(process.env.FORGE_QA_SHOTS_PORT ?? 4176);

// The config is loaded by the runner and every worker: the first load fixes the run for all
// (workers inherit the environment of the runner process).
process.env.QA_SHOTS_RUN ??= new Date().toISOString().replace(/[-:]/g, "").replace(/\..*/, "");
const RUN = process.env.QA_SHOTS_RUN;
process.env.QA_SHOTS_DIR = path.resolve(import.meta.dirname, "out/shots", RUN);

export default defineConfig({
  testDir: ".",
  testMatch: "shots.spec.ts",
  outputDir: "./out/shots-results",
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
