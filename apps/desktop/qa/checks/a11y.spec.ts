// axe WCAG 2.1 AA (0 serious/critical) on every route in light/dark x vibrancy on/off, on the
// scenarios, plus the explicit contrast of the fg tokens over the opaque sidebar fallback.
import {
  axeViolations,
  expect,
  ready,
  ROUTES,
  routeUrl,
  SCENARIOS,
  selftestUrl,
  sidebarContrastViolations,
  test,
  withMaterialBackdrop,
} from "./lib";

const THEMES = ["light", "dark"] as const;
const VIBRANCY = ["on", "off"] as const;

for (const route of ROUTES) {
  for (const theme of THEMES) {
    for (const vibrancy of VIBRANCY) {
      test(`axe: ${route} ${theme} vibrancy ${vibrancy}`, async ({ page }) => {
        await page.goto(routeUrl(route, { scenario: "populated", theme, vibrancy }));
        await ready(page);
        if (vibrancy === "on") await withMaterialBackdrop(page);
        expect(await axeViolations(page)).toEqual([]);
      });
    }
  }
}

// The data states of the shell: every scenario on the landing screen, both themes.
for (const scenario of SCENARIOS) {
  for (const theme of THEMES) {
    test(`axe: /dashboard scenario ${scenario} ${theme}`, async ({ page }) => {
      await page.goto(routeUrl("/dashboard", { scenario, theme, vibrancy: "off" }));
      await ready(page);
      expect(await axeViolations(page)).toEqual([]);
    });
  }
}

for (const theme of THEMES) {
  test(`sidebar fallback contrast: fg tokens ${theme}`, async ({ page }) => {
    await page.goto(routeUrl("/dashboard", { scenario: "populated", theme, vibrancy: "off" }));
    await ready(page);
    expect(await sidebarContrastViolations(page)).toEqual([]);
  });
}

test.describe("negative self-tests", () => {
  test("axe flags a nameless button and an image without alt", async ({ page }) => {
    await page.goto(selftestUrl("axe"));
    await ready(page);
    const found = await axeViolations(page);
    expect(found.join("\n")).toMatch(/button-name/);
  });

  test("axe passes the good page", async ({ page }) => {
    await page.goto(selftestUrl("ok"));
    await ready(page);
    expect(await axeViolations(page)).toEqual([]);
  });

  test("the sidebar contrast check flags a token equal to the background", async ({ page }) => {
    await page.goto(selftestUrl("sidebar-contrast", { vibrancy: "off" }));
    await ready(page);
    const found = await sidebarContrastViolations(page, "[data-qa-scope]");
    expect(found.join("\n")).toMatch(/--fg-secondary/);
  });

  test("the sidebar contrast check passes the good scope", async ({ page }) => {
    await page.goto(selftestUrl("ok", { vibrancy: "off" }));
    await ready(page);
    expect(await sidebarContrastViolations(page, "[data-qa-scope]")).toEqual([]);
  });
});
