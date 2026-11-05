// @vitest-environment node
// The fake-bridge Playwright suite (`pnpm e2e`) must not collect the suites that need a real
// Forgebox.exe/forgeboxd (e2e/real, e2e/live) nor the vitest files that live next to them:
// the M2-E1 gate (M2-E1-T21) found `e2e/live/parity.test.ts` breaking `pnpm e2e`.
import { describe, expect, it } from "vitest";
import fakeBridge from "../../playwright.config";
import live from "../../playwright.live.config";
import real from "../../playwright.real.config";

const ignored = (globs: unknown): string[] =>
  (Array.isArray(globs) ? globs : [globs]).filter((g): g is string => typeof g === "string");

describe("Playwright suites", () => {
  it("keeps the real-window and live folders out of the fake-bridge suite", () => {
    expect(fakeBridge.testDir).toBe("./e2e");
    expect(ignored(fakeBridge.testIgnore)).toEqual(expect.arrayContaining(["real/**", "live/**"]));
  });

  it("runs the real-window and live suites from their own folders", () => {
    expect(real.testDir).toBe("./e2e/real");
    expect(live.testDir).toBe("./e2e/live");
    expect(live.testMatch).toBe("**/*.live.spec.ts");
  });
});
