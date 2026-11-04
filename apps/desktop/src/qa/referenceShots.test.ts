// @vitest-environment node
// The curated reference screens of docs/design/screens (M2-E4-T7): one light and one dark PNG per
// screen of the shot matrix, 1280x800 at 100 %, <= 250 KB each and <= 8 MB in total.
import { describe, expect, it } from "vitest";
import {
  LIMITS,
  pngSize,
  readReferences,
  referenceProblems,
} from "../../scripts/reference-shots.mjs";
import { SCREENS } from "../../qa/matrix";

function png(width: number, height: number): Buffer {
  const buffer = Buffer.alloc(24);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(buffer);
  buffer.writeUInt32BE(width, 16);
  buffer.writeUInt32BE(height, 20);
  return buffer;
}

const ok = (name: string, bytes = 1000) => ({ name, bytes, size: pngSize(png(1280, 800)) });

describe("reference screens in docs/design/screens", () => {
  it("cover every screen of the matrix in light and dark within the limits", () => {
    const files = readReferences();
    expect(files.length).toBe(SCREENS.length * 2);
    expect(
      referenceProblems(
        files,
        SCREENS.map((s) => s.id),
      ),
    ).toEqual([]);
  });

  it("accept a good pair", () => {
    expect(referenceProblems([ok("sites-light.png"), ok("sites-dark.png")], ["sites"])).toEqual([]);
  });

  it("reject wrong dimensions, a non-PNG, an oversized file and a missing theme", () => {
    const problems = referenceProblems([
      { name: "php-light.png", bytes: 1000, size: pngSize(png(1600, 1000)) },
      { name: "php-dark.png", bytes: 1000, size: pngSize(Buffer.from("not a png, not at all")) },
      ok("logs-light.png", LIMITS.maxFileBytes + 1),
    ]);
    expect(problems.join("\n")).toMatch(/php-light\.png: 1600x1000/);
    expect(problems.join("\n")).toMatch(/php-dark\.png: not a PNG/);
    expect(problems.join("\n")).toMatch(/logs-light\.png: \d+ bytes >/);
    expect(problems.join("\n")).toMatch(/logs: missing logs-dark\.png/);
  });

  it("reject a total above 8 MB, a bad name and a missing screen", () => {
    const big = Array.from({ length: 40 }, (_, i) => [
      ok(`s${String(i)}-light.png`, LIMITS.maxFileBytes),
      ok(`s${String(i)}-dark.png`, LIMITS.maxFileBytes),
    ]).flat();
    expect(referenceProblems(big).join("\n")).toMatch(/^total \d+ bytes > /m);
    expect(referenceProblems([ok("Dashboard.png")], ["mail"]).join("\n")).toMatch(
      /Dashboard\.png: name is not[\s\S]*mail: no reference screen/,
    );
  });
});
