// The PHP screen on the qa build (M2-E2-T17, docs/07 section 5.5): three series columns that
// scroll sideways at 960 px, the CLI default radio, extension search and rollback, Xdebug with
// install progress and the pool req/s. Shots land in qa/out/php (gitignored).
import type { Page } from "@playwright/test";
import type { QaHandle } from "../src/qa/bootstrap";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/php";

function phpUrl(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/php?${params.toString()}`;
}

function column(page: Page, series: string) {
  return page.locator(`[data-php-series="${series}"]`);
}

test("three series scroll sideways at 960 px and fit at 1440 px", async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 600 });
  await page.goto(phpUrl());
  await expect(page.locator("[data-php-series]")).toHaveCount(3);
  await expect(page.locator("[data-php-series]")).toHaveText([/PHP 8\.2/, /PHP 8\.3/, /PHP 8\.4/], {
    useInnerText: false,
  });
  const region = page.getByTestId("php-columns");
  await expect(column(page, "8.3").getByTestId("pool-card")).toContainText("Running");
  const scrolls = await region.evaluate((el) => el.scrollWidth > el.clientWidth);
  expect(scrolls).toBe(true);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/php-960x600.png` });

  await page.setViewportSize({ width: 1440, height: 900 });
  await expect.poll(() => region.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/php-1440x900.png` });
});

test("the CLI radio switches the default version and follows tool.current_changed", async ({
  page,
}) => {
  await page.goto(phpUrl());
  const radio84 = column(page, "8.4").getByRole("radio", { name: "Default CLI" });
  const radio83 = column(page, "8.3").getByRole("radio", { name: "Default CLI" });
  await expect(radio83).toBeChecked();
  await expect(column(page, "8.4").locator("label[data-forge-cli]").first()).toHaveAttribute(
    "data-forge-cli",
    "forge use php 8.4",
  );
  await radio84.click();
  await expect(radio84).toBeChecked();
  await expect(radio83).not.toBeChecked();
});

test("extension search is instant and a failing toggle goes back with its cause", async ({
  page,
}) => {
  await page.goto(phpUrl());
  const ext = column(page, "8.3").getByTestId("extensions");
  await expect(ext.locator("[data-extension]")).toHaveCount(60);
  const ms = await ext.getByRole("searchbox").evaluate(async (input: HTMLInputElement) => {
    const start = performance.now();
    // React tracks the value itself: the native setter makes the input event look like typing.
    // eslint-disable-next-line @typescript-eslint/unbound-method -- called with `.call(input)`
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(input, "pdo");
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    return performance.now() - start;
  });
  await expect(ext.locator("[data-extension]")).toHaveCount(6);
  // One frame at 60 Hz is 16.7 ms: filtering and committing 60 rows fits in it.
  expect(ms).toBeLessThan(50);
  await ext.getByRole("searchbox").fill("sodium");
  const toggle = ext.getByRole("switch", { name: "sodium extension" });
  await expect(toggle).not.toBeChecked();
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect(ext.getByText("Could not change sodium: it was put back.")).toBeVisible();
  await expect(toggle).not.toBeChecked();
  await ext.getByRole("searchbox").fill("intl");
  const intl = ext.getByRole("switch", { name: "intl extension" });
  await expect(ext.locator("[data-extension=intl]")).toHaveAttribute(
    "data-forge-cli",
    "forge php ext disable 8.3 intl",
  );
  await intl.click();
  await expect(intl).not.toBeChecked();
  await expect(ext.locator("[data-extension=intl]")).not.toHaveAttribute("data-pending", "true");
  await expect(intl).not.toBeChecked();
});

test("Xdebug trigger without the extension shows install progress, then trigger mode", async ({
  page,
}) => {
  await page.goto(phpUrl());
  const xdebug = column(page, "8.4").getByTestId("xdebug");
  await expect(xdebug).toHaveAttribute("data-mode", "off");
  await xdebug.getByRole("radio", { name: "Trigger" }).click();
  await expect(xdebug.locator("[data-install-job]")).toBeVisible();
  await expect(xdebug.getByText("Installing Xdebug for PHP 8.4")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/xdebug-installing.png` });
  await expect(xdebug).toHaveAttribute("data-mode", "trigger");
  await expect(xdebug.locator("[data-install-job]")).toHaveCount(0);
  await expect(xdebug.locator("[data-forge-cli]").first()).toHaveAttribute(
    "data-forge-cli",
    "forge php xdebug 8.4 trigger",
  );
});

test("the pool card shows req/s from the requests_total delta with tabular figures", async ({
  page,
}) => {
  await page.goto(phpUrl());
  await expect(column(page, "8.3").getByTestId("pool-rate")).toContainText("— req/s");
  // A pool that serves exactly 20 requests every second, whatever the poll interval.
  await page.evaluate(async () => {
    const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
    const handlers = qa.bridge.scenario.handlers as Record<
      string,
      (params: unknown, ctx: unknown) => Promise<unknown>
    >;
    const list = (await handlers["php.list"]?.({}, {})) as {
      versions: { series: string; pool: Record<string, unknown> | null }[];
    };
    const started = Date.now();
    handlers["services.status"] = () =>
      Promise.resolve({
        instances: [],
        pools: list.versions.flatMap((v) =>
          v.pool === null
            ? []
            : [{ ...v.pool, requests_total: 100 + (20 * (Date.now() - started)) / 1000 }],
        ),
      });
  });
  const rate = column(page, "8.3").getByTestId("pool-rate");
  await expect(rate).toHaveAttribute("data-value", /^(19|20|21)(\.\d+)?$/, { timeout: 15_000 });
  await expect(rate).toContainText(/^(19|20|21)\.\d req\/s$/);
  await expect(rate.locator("..")).toHaveClass(/tabular-nums/);
});

test("the sites count opens the Sites list filtered by series", async ({ page }) => {
  await page.goto(phpUrl());
  const link = column(page, "8.3").getByTestId("series-sites");
  const count = Number((await link.textContent())?.match(/\d+/)?.[0]);
  expect(count).toBeGreaterThan(0);
  await link.click();
  await expect(page).toHaveURL(/\/sites\?php=/);
  await expect(page.getByTestId("php-filter")).toContainText("PHP 8.3");
  await expect(page.locator("[data-site-card]")).toHaveCount(count);
});

test("empty scenario: install 8.4 opens the catalog on php", async ({ page }) => {
  await page.goto(phpUrl({ scenario: "empty" }));
  await expect(page.getByText("No PHP installed")).toBeVisible();
  await page.getByRole("button", { name: "Install PHP 8.4" }).click();
  await expect(page).toHaveURL(/\/catalog\/php/);
});
