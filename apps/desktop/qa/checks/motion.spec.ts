// Reduced motion: every animation <= 100 ms and opacity-only. Routes are loaded with the
// preference on; the gallery also opens every surface (sheet, popover, toast).
import {
  emulate,
  expect,
  motionViolations,
  ready,
  recordAnimations,
  ROUTES,
  routeUrl,
  selftestUrl,
  test,
} from "./lib";

for (const route of ROUTES) {
  test(`reduced motion: ${route}`, async ({ page }) => {
    await recordAnimations(page);
    await emulate(page, { reducedMotion: true });
    await page.goto(routeUrl(route, { scenario: "populated", motion: "reduced" }));
    await ready(page);
    await page.waitForTimeout(500);
    expect(await motionViolations(page)).toEqual([]);
  });
}

test("reduced motion: gallery surfaces (sheet, popover)", async ({ page }) => {
  await recordAnimations(page);
  await emulate(page, { reducedMotion: true });
  await page.goto(routeUrl("/__gallery", { scenario: "populated", motion: "reduced" }));
  await ready(page);
  const triggers = page.locator("[aria-haspopup=dialog]");
  const count = await triggers.count();
  expect(count).toBeGreaterThanOrEqual(2);
  for (let i = 0; i < count; i += 1) {
    await triggers.nth(i).click();
    await page.waitForTimeout(300);
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);
  }
  expect(await motionViolations(page)).toEqual([]);
});

test.describe("negative self-tests", () => {
  test("the good page has no slow or non-opacity animation", async ({ page }) => {
    await recordAnimations(page);
    await emulate(page, { reducedMotion: true });
    await page.goto(selftestUrl("ok", { motion: "reduced" }));
    await ready(page);
    await page.getByRole("button", { name: "Open sheet" }).click();
    await page.waitForTimeout(400);
    expect(await motionViolations(page)).toEqual([]);
  });

  test("flags a 600 ms transform animation", async ({ page }) => {
    await recordAnimations(page);
    await emulate(page, { reducedMotion: true });
    await page.goto(selftestUrl("motion", { motion: "reduced" }));
    await ready(page);
    await page.waitForTimeout(300);
    const found = (await motionViolations(page)).join("\n");
    expect(found).toMatch(/600 ms/);
    expect(found).toMatch(/transform/);
  });
});
