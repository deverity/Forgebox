// Text on --accent >= 4.5:1 for the Windows system accents, in both themes.
import {
  accentContrastViolations,
  accentStateContrastViolations,
  expect,
  ready,
  routeUrl,
  selftestUrl,
  SYSTEM_ACCENTS,
  test,
} from "./lib";

const PRIMARY = "[data-variant-group=primary] button:not([disabled])";

for (const theme of ["light", "dark"] as const) {
  test(`primary buttons on every system accent (${theme})`, async ({ page }) => {
    await page.goto(routeUrl("/__gallery", { theme, vibrancy: "off" }));
    await ready(page);
    expect(await accentContrastViolations(page, PRIMARY)).toEqual([]);
  });
}

for (const theme of ["light", "dark"] as const) {
  test(`primary button text stays >= 4.5:1 hovered and pressed on every system accent (${theme})`, async ({
    page,
  }) => {
    await page.goto(routeUrl("/__gallery", { theme, vibrancy: "off" }));
    await ready(page);
    expect(await accentStateContrastViolations(page, PRIMARY)).toEqual([]);
  });
}

test("the accents under test are the four of docs/07", () => {
  expect(SYSTEM_ACCENTS).toEqual(["#0078D4", "#FFB900", "#E81123", "#00CC6A"]);
});

test.describe("negative self-tests", () => {
  for (const theme of ["light", "dark"] as const) {
    test(`flags white text hard-coded on --accent (${theme})`, async ({ page }) => {
      await page.goto(selftestUrl("accent-contrast", { theme }));
      await ready(page);
      const found = await accentContrastViolations(page, "[data-qa-on-accent]");
      expect(found.join("\n")).toMatch(/#FFB900/);
    });

    test(`passes text on the --fg-on-accent token (${theme})`, async ({ page }) => {
      await page.goto(selftestUrl("ok", { theme }));
      await ready(page);
      expect(await accentContrastViolations(page, "[data-qa-on-accent]")).toEqual([]);
    });
  }
});
