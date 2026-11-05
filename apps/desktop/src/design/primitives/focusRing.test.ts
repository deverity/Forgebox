import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { FOCUS_RING } from "./shared";

// Tailwind 4: `outline-none`/`outline-hidden` set --tw-outline-style: none and `outline-2`
// reads that variable, so the ring stays invisible unless `outline-solid` is applied on focus.
describe("focus ring", () => {
  it("restores a solid outline style on focus-visible", () => {
    expect(FOCUS_RING).toContain("focus-visible:outline-solid");
    expect(FOCUS_RING).toContain("focus-visible:outline-accent");
  });

  it("no primitive hides the outline without the solid restore", () => {
    const dir = import.meta.dirname;
    const files = readdirSync(dir).filter(
      (n) => /\.tsx?$/.test(n) && !n.includes(".test.") && n !== "shared.ts",
    );
    for (const f of files) {
      const lines = readFileSync(join(dir, f), "utf8").split(/\r?\n/);
      // Listbox options are the exception: their focus indicator is the highlighted background.
      const bad = lines.filter(
        (l) => /outline-(none|hidden)/.test(l) && !l.includes("data-[highlighted]"),
      );
      expect(bad, f).toEqual([]);
    }
  });

  // Forced colors (Windows high contrast) replace backgrounds; these paint state with them.
  it.each(["Toggle.tsx", "Slider.tsx", "StatusDot.tsx"])(
    "%s opts out of forced-color-adjust",
    (f) => {
      expect(readFileSync(join(import.meta.dirname, f), "utf8")).toContain(
        "forced-color-adjust-none",
      );
    },
  );
});
