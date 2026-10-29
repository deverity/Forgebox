// The real-window e2e (ADR-0036): attaches to a running Forgebox.exe over WebView2's CDP port.
// Not meant to be run by hand: scripts/e2e-desktop.ps1 builds, launches the app with
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port> (test env only, never in
// code or release config) and a temp portable root, then runs this config with the FORGE_E2E_*
// variables set.
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e/real",
  outputDir: "./qa/out/test-results-real",
  fullyParallel: false,
  workers: 1,
  timeout: 120_000,
  reporter: [["list"]],
});
