import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// `tauri dev` loads devUrl (tauri.conf.json) and requires this exact port.
const DEV_PORT = 1420;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Keep the Rust compiler output visible in the `tauri dev` terminal.
  clearScreen: false,
  server: {
    port: DEV_PORT,
    strictPort: true,
    // src-tauri/target holds hundreds of thousands of files; never watch them.
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "es2022" },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
