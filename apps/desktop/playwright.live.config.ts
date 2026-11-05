// The E2 live suite (M2-E2-T20): attaches to a running Forgebox.exe (debug) over WebView2's CDP
// port. Not meant to be run by hand: scripts/gui-e2e.ps1 builds, creates the fixture root,
// launches the app and runs this config with the FORGE_E2E_* variables set.
import { defineConfig } from "@playwright/test";

// FORGE_E2E_SUITE=e3 (scripts/gui-e2e.ps1 -Suite e3, M2-E3-T21) runs the E3 screens instead.
const e3 = process.env.FORGE_E2E_SUITE === "e3";

export default defineConfig({
  testDir: e3 ? "./e2e/live/e3" : "./e2e/live",
  testIgnore: e3 ? [] : ["e3/**"],
  // parity.test.ts is a vitest file (the scan validator's own tests), not part of this run.
  testMatch: "**/*.live.spec.ts",
  outputDir: "./qa/out/test-results-live",
  fullyParallel: false,
  workers: 1,
  timeout: 180_000,
  reporter: [["list"]],
});
