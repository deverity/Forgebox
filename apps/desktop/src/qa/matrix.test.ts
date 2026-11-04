// The screenshot matrix is checked against the router (M2-E4-T2): a route nobody listed fails.
import { describe, expect, it } from "vitest";
import {
  EXCLUDED_ROUTES,
  SCREENS,
  buildMatrix,
  cellFile,
  cellName,
  missingRoutes,
  pngSize,
} from "../../qa/matrix";
import { createTestRouter } from "../app/router";

describe("screenshot matrix", () => {
  const routes = Object.keys(createTestRouter().routesByPath);

  it("has an entry (or a reasoned exclusion) for every router route", () => {
    expect(routes.length).toBeGreaterThan(10);
    expect(missingRoutes(routes)).toEqual([]);
  });

  it("fails for a route with no matrix entry", () => {
    expect(missingRoutes([...routes, "/brand-new-screen"])).toEqual(["/brand-new-screen"]);
  });

  it("lists no screen or exclusion the router does not have", () => {
    const known = new Set(routes);
    for (const screen of SCREENS) expect(known.has(screen.route), screen.route).toBe(true);
    for (const route of Object.keys(EXCLUDED_ROUTES)) {
      // The gallery and self-test routes exist only in some build modes (not in vitest's).
      if (route !== "/__gallery" && route !== "/__charts" && route !== "/qa/selftest") {
        expect(known.has(route), route).toBe(true);
      }
    }
  });

  it("names files <screen>/<scenario>-<theme>-<dpr>-<vib>-<motion>-<locale>-<w>x<h>.png", () => {
    const [cell] = buildMatrix(
      "full",
      SCREENS.filter((s) => s.id === "dashboard"),
    );
    expect(cell && cellFile(cell)).toBe(
      "dashboard/populated-light-1x-vibon-full-pt-BR-1280x800.png",
    );
  });

  it("covers the core matrix and the extras, without duplicates", () => {
    const cells = buildMatrix("full");
    const files = cells.map(cellFile);
    expect(new Set(files).size).toBe(files.length);
    const dash = cells.filter((c) => c.screen.id === "dashboard");
    const names = dash.map(cellName);
    for (const theme of ["light", "dark"]) {
      for (const dpr of ["1x", "1.25x", "1.5x"]) {
        expect(names).toContain(`populated-${theme}-${dpr}-vibon-full-pt-BR-1280x800`);
      }
    }
    expect(names).toContain("populated-light-1x-viboff-full-pt-BR-1280x800");
    expect(names).toContain("populated-light-1x-vibon-reduced-pt-BR-1280x800");
    expect(names).toContain("populated-light-1x-vibon-full-en-US-1280x800");
    expect(names).toContain("populated-light-1x-vibon-full-pseudo-1280x800");
    expect(names).toContain("populated-light-2x-vibon-full-pt-BR-1280x800");
    expect(names).toContain("populated-forced-1x-vibon-full-pt-BR-1280x800");
    expect(names).toContain("populated-dark-1x-vibon-full-pt-BR-1093x574");
  });

  it("keeps the CI subset small and expects PNG = viewport x DPR", () => {
    expect(buildMatrix("ci").length).toBeLessThan(buildMatrix("full").length / 5);
    const cell = buildMatrix("full").find((c) => c.dpr === 1.25);
    expect(cell && pngSize(cell)).toEqual({ w: 1600, h: 1000 });
  });
});
