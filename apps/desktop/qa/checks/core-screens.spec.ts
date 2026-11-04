// M2-E4-T9: measured regressions of the core-screen findings (docs/design/qa/M2-E4-report.md
// section 2.3): R-09/R-10/R-16 (no endless skeleton: a failed or unavailable list shows its cause
// and an action), R-13/R-17 (dashboard tiles keep their content inside the card and the port
// visible), R-19 (service columns align across rows), R-21/R-22 (PHP fits the width and the
// document does not scroll).
import { expect, ready, routeUrl, test } from "./lib";

const SITE = "/sites/01QAS1TE000000000000000001";
const SETTLE_MS = 3200;

// Screens whose data comes from a list call, in the scenarios that make that call fail or be
// unavailable (the QA "errors" scenario fails every call; "not-implemented" lists the methods).
const LISTS = [
  { name: "sites", path: "/sites" },
  { name: "site detail", path: SITE },
  { name: "services", path: "/services" },
  { name: "php", path: "/php" },
  { name: "tool page", path: "/catalog/php" },
] as const;

for (const scenario of ["errors", "not-implemented"] as const) {
  for (const list of LISTS) {
    test(`no endless skeleton: ${list.name} in ${scenario}`, async ({ page }) => {
      await page.goto(routeUrl(list.path, { scenario, locale: "en-US" }));
      await ready(page);
      // The notice replaces the skeleton once the first load settles.
      await expect(page.locator("main [data-error-notice]").first()).toBeVisible({
        timeout: 8_000,
      });
      expect(await page.locator("main [aria-busy='true']").count()).toBe(0);
    });
  }
}

test("daemon down: the site detail says so instead of a skeleton", async ({ page }) => {
  await page.goto(routeUrl(SITE, { scenario: "daemon-down", locale: "en-US" }));
  await ready(page);
  await expect(page.locator("main").getByText("No connection to the daemon")).toBeVisible();
  expect(await page.locator("main [aria-busy='true']").count()).toBe(0);
});

test("dashboard tiles keep charts inside the card and the port visible (R-13, R-17)", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(routeUrl("/dashboard", { scenario: "populated", locale: "en-US" }));
  await ready(page);
  await page.waitForTimeout(SETTLE_MS);
  const result = await page.evaluate(() => {
    const out: string[] = [];
    for (const tile of document.querySelectorAll<HTMLElement>("[data-tile]")) {
      const box = tile.getBoundingClientRect();
      for (const el of tile.querySelectorAll<HTMLElement | SVGElement>("*")) {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) continue;
        if (r.bottom > box.bottom + 0.5 || r.right > box.right + 0.5) {
          out.push(`${tile.dataset.tile ?? ""}: <${el.tagName.toLowerCase()}> past the card`);
        }
      }
      const port = tile.querySelector<HTMLElement>("[data-testid='tile-port']");
      if (port !== null && port.scrollWidth > port.clientWidth) {
        out.push(`${tile.dataset.tile ?? ""}: the port is cut`);
      }
    }
    return out;
  });
  expect(result).toEqual([]);
});

test("service columns line up across rows and groups (R-19)", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(routeUrl("/services", { scenario: "populated", locale: "en-US" }));
  await ready(page);
  await expect(page.locator("[data-service-row]").first()).toBeVisible();
  const starts = await page.evaluate(() => {
    const columns: number[][] = [[], [], [], []];
    for (const row of document.querySelectorAll<HTMLElement>("[data-service-row]")) {
      const cells = [...row.children].slice(1, 5) as HTMLElement[];
      cells.forEach((cell, index) =>
        columns[index]?.push(Math.round(cell.getBoundingClientRect().x)),
      );
    }
    return columns.map((xs) => [...new Set(xs)]);
  });
  for (const xs of starts) expect(xs).toHaveLength(1);
});

for (const size of [
  { width: 960, height: 600 },
  { width: 1280, height: 800 },
] as const) {
  test(`php fits ${String(size.width)}x${String(size.height)} and the document does not scroll (R-21, R-22)`, async ({
    page,
  }) => {
    await page.setViewportSize(size);
    await page.goto(routeUrl("/php", { scenario: "populated", locale: "en-US" }));
    await ready(page);
    await expect(page.getByTestId("php-columns")).toBeVisible();
    await page.waitForTimeout(800);
    const probe = await page.evaluate(() => {
      const region = document.querySelector<HTMLElement>("[data-testid='php-columns']");
      return {
        docScroll: document.documentElement.scrollHeight - window.innerHeight,
        docWide: document.documentElement.scrollWidth - window.innerWidth,
        regionOverflow: region === null ? -1 : region.scrollWidth - region.clientWidth,
      };
    });
    expect(probe.docScroll).toBeLessThanOrEqual(0);
    expect(probe.docWide).toBeLessThanOrEqual(0);
    expect(probe.regionOverflow).toBeLessThanOrEqual(1);
  });
}
