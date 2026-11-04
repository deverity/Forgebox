// The E2 live suite (M2-E2-T20): attaches to a running Forgebox.exe (debug) over WebView2's CDP
// port. Not meant to be run by hand: scripts/gui-e2e.ps1 builds, creates the fixture root,
// launches the app and runs this config with the FORGE_E2E_* variables set.
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e/live",
  outputDir: "./qa/out/test-results-live",
  fullyParallel: false,
  workers: 1,
  timeout: 180_000,
  reporter: [["list"]],
});
