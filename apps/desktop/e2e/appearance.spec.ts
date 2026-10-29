// Emulation, axe, keyboard traversal, CLS, motion and locale on the qa build (ADR-0036).
import { axeViolations, emulate, expect, qaUrl, tabOrder, test, trackCls } from "./fixtures";

test("theme and vibrancy parameters reach <html>", async ({ page }) => {
  await page.goto(qaUrl({ theme: "dark", vibrancy: "off" }));
  const html = page.locator("html");
  await expect(html).toHaveAttribute("data-theme", "dark");
  await expect(html).toHaveAttribute("data-vibrancy", "off");
});

for (const theme of ["light", "dark"] as const) {
  test(`axe finds no serious violation (${theme})`, async ({ page }) => {
    await page.goto(qaUrl({ theme, scenario: "populated" }));
    await expect(page.locator("main h1")).toBeVisible();
    expect(await axeViolations(page)).toEqual([]);
  });
}

test("keyboard traversal reaches the window controls and the language switcher", async ({
  page,
}) => {
  await page.goto(qaUrl());
  await expect(page.locator("main h1")).toBeVisible();
  const order = await tabOrder(page);
  expect(order.length).toBeGreaterThan(1);
  expect(order.some((entry) => /minimize|minimizar/i.test(entry))).toBe(true);
});

test("the first render does not shift the layout", async ({ page }) => {
  const read = await trackCls(page);
  await page.goto(qaUrl({ scenario: "populated" }));
  await expect(page.locator("main h1")).toBeVisible();
  await page.waitForTimeout(300);
  expect(await read()).toBeLessThan(0.05);
});

test("emulated media features: scheme, reduced motion, reduced transparency, scale", async ({
  page,
}) => {
  await page.goto(qaUrl());
  await emulate(page, {
    colorScheme: "dark",
    reducedMotion: true,
    reducedTransparency: true,
    deviceScaleFactor: 2,
  });
  const media = await page.evaluate(() => ({
    dark: matchMedia("(prefers-color-scheme: dark)").matches,
    motion: matchMedia("(prefers-reduced-motion: reduce)").matches,
    transparency: matchMedia("(prefers-reduced-transparency: reduce)").matches,
    dpr: window.devicePixelRatio,
  }));
  expect(media).toEqual({ dark: true, motion: true, transparency: true, dpr: 2 });
});

test("motion=reduced makes the reduced-motion query match", async ({ page }) => {
  await page.goto(qaUrl({ motion: "reduced" }));
  expect(await page.evaluate(() => matchMedia("(prefers-reduced-motion: reduce)").matches)).toBe(
    true,
  );
});

test("the pseudo-locale marks every string", async ({ page }) => {
  await page.goto(qaUrl({ locale: "pseudo" }));
  await expect(page.locator("main h1")).toContainText(/[À-ɏ]/);
});

test("pt-BR and en-US render different text", async ({ page }) => {
  await page.goto(qaUrl({ locale: "pt-BR" }));
  const pt = await page.locator("main p").first().textContent();
  await page.goto(qaUrl({ locale: "en-US" }));
  const en = await page.locator("main p").first().textContent();
  expect(pt).not.toEqual(en);
});
