// @vitest-environment node
// The size budgets of `pnpm size` (scripts/bundle-size.mjs, M2-E1-T21, docs/07 section 10).
import { describe, expect, it } from "vitest";
import { BUDGETS, EXE_TARGET_MB, gzipBytes, initialAssets } from "../../scripts/bundle-size.mjs";

const HTML = `<!doctype html><html><head>
<link rel="icon" href="data:," />
<script type="module" crossorigin src="/assets/index-AAA.js"></script>
<link rel="modulepreload" crossorigin href="/assets/vendor-BBB.js">
<link rel="modulepreload" crossorigin href="/assets/vendor-BBB.js">
<link rel="stylesheet" crossorigin href="/assets/index-CCC.css">
<link rel="preload" as="font" href="/assets/font-DDD.woff2">
<script src="/assets/classic-EEE.js"></script>
</head><body><div id="root"></div></body></html>`;

describe("initial bundle of index.html", () => {
  it("is the module entry plus the JS modulepreload links, once each, in order", () => {
    expect(initialAssets(HTML)).toEqual(["/assets/index-AAA.js", "/assets/vendor-BBB.js"]);
  });

  it("ignores stylesheets, fonts, icons and classic scripts", () => {
    const assets = initialAssets(HTML);
    expect(assets.some((a) => a.endsWith(".css") || a.endsWith(".woff2"))).toBe(false);
    expect(assets).not.toContain("/assets/classic-EEE.js");
  });

  it("is empty for a page without module scripts", () => {
    expect(initialAssets("<html><head></head></html>")).toEqual([]);
  });
});

describe("budgets", () => {
  it("gzip -9 shrinks repetitive text", () => {
    const text = Buffer.from("forgebox ".repeat(1000));
    expect(gzipBytes(text)).toBeLessThan(text.length / 20);
  });

  it("keep the exe target of docs/08 below the exe budget", () => {
    expect(EXE_TARGET_MB).toBe(8);
    expect(BUDGETS.exeMb).toBeGreaterThanOrEqual(EXE_TARGET_MB);
    expect(BUDGETS.initialJsGzipKb).toBeGreaterThan(0);
  });
});
