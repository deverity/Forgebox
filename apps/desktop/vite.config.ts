import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { searchForWorkspaceRoot } from "vite";
import { defineConfig } from "vitest/config";

// `tauri dev` loads devUrl (tauri.conf.json) and requires this exact port.
const DEV_PORT = 1420;

export default defineConfig(({ mode }) => ({
  plugins: [react(), tailwindcss()],
  // Keep the Rust compiler output visible in the `tauri dev` terminal.
  clearScreen: false,
  server: {
    port: DEV_PORT,
    strictPort: true,
    // src-tauri/target holds hundreds of thousands of files; never watch them.
    watch: { ignored: ["**/src-tauri/**"] },
    // The QA fixtures bundle the official registry's icons (`dev:qa`, M2-E1-T24).
    fs: {
      allow: [
        searchForWorkspaceRoot(process.cwd()),
        "../../registry/icons",
        "../../registry/tools",
        "../../registry/stacks",
      ],
    },
  },
  // `--mode qa` (ADR-0036) builds next to the real app, never over dist/ (Tauri embeds it).
  build: { target: "es2022", outDir: mode === "qa" ? "dist-qa" : "dist" },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}", "qa/parity/*.test.ts", "e2e/live/*.test.ts"],
    // Above Testing Library's asyncUtilTimeout (5 s, src/test/setup.ts): a findBy that waits its
    // full window must fail with its own message, not end the test by timeout (ADR-0036).
    testTimeout: 20_000,
    hookTimeout: 20_000,
  },
}));
