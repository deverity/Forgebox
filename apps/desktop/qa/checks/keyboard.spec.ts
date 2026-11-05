// Keyboard: Tab reaches every control with a visible --accent focus ring, Escape closes sheets
// and popovers and returns the focus, and the section 8 shortcuts work.
import {
  escapeViolations,
  expect,
  keyboardViolations,
  ready,
  ROUTES,
  routeUrl,
  selftestUrl,
  shortcutViolations,
  test,
} from "./lib";
import { SECTION_8, SELFTEST_SHORTCUT } from "./shortcuts";

for (const route of ROUTES) {
  for (const theme of ["light", "dark"] as const) {
    test(`tab order and focus ring: ${route} ${theme}`, async ({ page }) => {
      await page.goto(routeUrl(route, { scenario: "populated", theme, vibrancy: "off" }));
      await ready(page);
      expect(await keyboardViolations(page)).toEqual([]);
    });
  }

  test(`escape closes popups and returns focus: ${route}`, async ({ page }) => {
    await page.goto(routeUrl(route, { scenario: "populated", vibrancy: "off" }));
    await ready(page);
    expect(await escapeViolations(page)).toEqual([]);
  });
}

for (const shortcut of SECTION_8) {
  test(`shortcut ${shortcut.keys} (${shortcut.label})`, async ({ page }) => {
    test.skip(shortcut.effect === null, `not implemented yet (${shortcut.pending ?? "?"})`);
    await page.goto(routeUrl("/dashboard", { scenario: "populated" }));
    await ready(page);
    expect(await shortcutViolations(page, [shortcut])).toEqual([]);
  });
}

test.describe("negative self-tests", () => {
  test("the good page passes tab order, escape and shortcut", async ({ page }) => {
    await page.goto(selftestUrl("ok"));
    await ready(page);
    expect(await keyboardViolations(page)).toEqual([]);
    expect(await escapeViolations(page)).toEqual([]);
    expect(await shortcutViolations(page, [SELFTEST_SHORTCUT])).toEqual([]);
  });

  test("flags a control Tab never reaches", async ({ page }) => {
    await page.goto(selftestUrl("unreachable"));
    await ready(page);
    expect((await keyboardViolations(page)).join("\n")).toMatch(/Tab never reaches/);
  });

  test("flags a focused control without a visible ring", async ({ page }) => {
    await page.goto(selftestUrl("no-focus-ring"));
    await ready(page);
    expect((await keyboardViolations(page)).join("\n")).toMatch(/no visible --accent focus ring/);
  });

  test("flags a surface that Escape does not close", async ({ page }) => {
    await page.goto(selftestUrl("escape"));
    await ready(page);
    expect((await escapeViolations(page)).join("\n")).toMatch(/Escape does not close/);
  });

  test("flags a shortcut without effect", async ({ page }) => {
    await page.goto(selftestUrl("shortcut"));
    await ready(page);
    expect((await shortcutViolations(page, [SELFTEST_SHORTCUT])).join("\n")).toMatch(
      /has no effect/,
    );
  });
});
