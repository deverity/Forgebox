// The README screenshots (M2-E4-T12): every relative link resolves, each image respects the size
// limits and the captions match the capture list (qa/readme-shots.ts).
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { FORBIDDEN_IN_TEXT, MAX_BYTES, MAX_WIDTH, SHOTS, imagePath } from "../../qa/readme-shots";

const REPO_ROOT = path.resolve(import.meta.dirname, "../../../..");
const README = readFileSync(path.join(REPO_ROOT, "README.md"), "utf8");

/** Markdown image/link targets and HTML `src`/`href` values that are relative paths. */
function relativeTargets(text: string): string[] {
  const targets = [
    ...[...text.matchAll(/!?\[[^\]]*\]\(([^)\s]+)\)/g)].map((m) => m[1] ?? ""),
    ...[...text.matchAll(/(?:src|href)="([^"]+)"/g)].map((m) => m[1] ?? ""),
  ];
  return targets
    .filter((t) => t !== "" && !/^(https?:|mailto:|#)/.test(t))
    .map((t) => t.split("#")[0] ?? t);
}

describe("README screenshots", () => {
  it("every relative link and image in README.md resolves to a file", () => {
    const targets = relativeTargets(README);
    expect(targets.length).toBeGreaterThan(10);
    const missing = targets.filter((t) => !existsSync(path.join(REPO_ROOT, t)));
    expect(missing).toEqual([]);
  });

  it("detects a broken link (negative case)", () => {
    const broken = relativeTargets("![x](docs/assets/screenshots/nao-existe.png)");
    expect(broken.filter((t) => !existsSync(path.join(REPO_ROOT, t)))).toHaveLength(1);
  });

  it("shows the hero image and every capture with its pt-BR and en-US captions", () => {
    expect(README).toContain("docs/design/brand/forgebox-icon-1024.png");
    expect(README).toContain("## Capturas de tela");
    for (const shot of SHOTS) {
      expect(README, shot.file).toContain(`](${imagePath(shot)})`);
      expect(README, `${shot.file} pt`).toContain(shot.pt);
      expect(README, `${shot.file} en`).toContain(shot.en);
    }
  });

  it("has 5 to 9 captures, dark first and at least one light", () => {
    expect(SHOTS.length).toBeGreaterThanOrEqual(5);
    expect(SHOTS.length).toBeLessThanOrEqual(9);
    expect(SHOTS[0]?.theme).toBe("dark");
    expect(SHOTS.some((s) => s.theme === "light")).toBe(true);
  });

  it.each(SHOTS.map((s) => [s.file, imagePath(s)] as const))(
    "%s is a PNG within the byte and width limits",
    (_name, rel) => {
      const file = path.join(REPO_ROOT, rel);
      expect(statSync(file).size).toBeLessThanOrEqual(MAX_BYTES);
      const png = readFileSync(file);
      expect(png.subarray(1, 4).toString("latin1")).toBe("PNG");
      expect(png.readUInt32BE(16)).toBeLessThanOrEqual(MAX_WIDTH);
    },
  );

  it("keeps personal data out of the README", () => {
    for (const re of FORBIDDEN_IN_TEXT) expect(README).not.toMatch(re);
  });

  it("Status and Desenvolvimento reflect M1 and M2", () => {
    expect(README).not.toContain("Fase de especificação");
    expect(README).toContain("M1");
    expect(README).toContain("M2");
    expect(README).toContain("pnpm -C apps/desktop qa:readme-shots");
    expect(README).toContain("tauri dev");
    expect(README).not.toContain("placeholders até");
  });
});
