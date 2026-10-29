// @vitest-environment node
// qa code must never reach a production bundle (ADR-0036): build both modes into temp
// folders and scan every emitted file for the sentinel and the fixture marker.
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build } from "vite";
import { afterAll, describe, expect, it } from "vitest";
import { QA_SENTINEL } from "./bootstrap";

const out = mkdtempSync(join(tmpdir(), "forgebox-sentinel-"));
afterAll(() => {
  rmSync(out, { recursive: true, force: true });
});

async function emit(mode: string): Promise<string> {
  const outDir = join(out, mode);
  await build({
    mode,
    logLevel: "silent",
    build: { outDir, emptyOutDir: true },
  });
  return outDir;
}

function scan(dir: string, needle: string): string[] {
  return readdirSync(dir, { recursive: true, encoding: "utf8" })
    .filter((f) => /\.(js|css|html|map)$/.test(f))
    .filter((f) => readFileSync(join(dir, f), "utf8").includes(needle));
}

describe("qa code and production builds", () => {
  it("the production bundle contains no qa sentinel", async () => {
    const dir = await emit("production");
    expect(scan(dir, QA_SENTINEL)).toEqual([]);
    expect(scan(dir, "__FORGEBOX_QA__")).toEqual([]);
    expect(scan(dir, "QA_SCENARIO_NAMES")).toEqual([]);
  }, 180_000);

  it("the qa bundle does contain it (the scan is not vacuous)", async () => {
    const dir = await emit("qa");
    expect(scan(dir, QA_SENTINEL).length).toBeGreaterThan(0);
  }, 180_000);
});
