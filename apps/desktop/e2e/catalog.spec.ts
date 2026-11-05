// The Catalog screens on the qa build (M2-E2-T18): list and filters, the hostile icon and
// Markdown payloads, install with progress and cancel, the stale/error states of
// `tools.releases` and a stack install behind its plan total. Data: qa/fixtures/scenarios/catalog.ts.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

function catalogUrl(path = "/catalog"): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US" });
  return `${path}?${params.toString()}`;
}

const pwned = (page: Page) =>
  page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned);

test("lists tools and stacks, searches with a debounce and filters", async ({ page }) => {
  await page.goto(catalogUrl());
  const cards = page.locator("[data-catalog-card]");
  await expect(cards).toHaveCount(10);
  await expect(page.locator('[data-catalog-card][data-kind="stack"]')).toHaveCount(3);

  await page.getByRole("searchbox", { name: "Search the catalog" }).fill("mailp");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toHaveAttribute("data-catalog-card", "mailpit");
  await page.getByRole("searchbox", { name: "Search the catalog" }).fill("");
  await expect(cards).toHaveCount(10);

  await page.getByRole("radio", { name: "Updates available" }).click();
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toHaveAttribute("data-catalog-card", "mysql");
});

test("a hostile icon is only an <img> with a data URI", async ({ page }) => {
  await page.goto(catalogUrl());
  const card = page.locator('[data-catalog-card="shady"]');
  // Icons load lazily: the card below the fold fetches its icon once it scrolls into view.
  await card.scrollIntoViewIfNeeded();
  await expect(card.locator('img[src^="data:image/svg+xml"]')).toBeVisible();
  // The card keeps no inline SVG of its own icon, no script and no handler attributes.
  expect(await card.locator("[data-tool-icon] svg, [data-tool-icon] script").count()).toBe(0);
  const handlers = await card.evaluate((element) =>
    [...element.querySelectorAll("*")].flatMap((el) =>
      el.getAttributeNames().filter((name) => name.startsWith("on")),
    ),
  );
  expect(handlers).toEqual([]);
  expect(await pwned(page)).toBeUndefined();
});

test("Markdown with raw HTML renders as text", async ({ page }) => {
  await page.goto(catalogUrl("/catalog/php"));
  const markdown = page.getByTestId("markdown");
  await expect(markdown).toContainText("<script>");
  expect(await markdown.locator("script, img").count()).toBe(0);
  await expect(markdown.getByRole("link", { name: "documentation" })).toBeVisible();
  expect(await pwned(page)).toBeUndefined();
});

test("installs a version and variant with progress and the equivalent command", async ({
  page,
}) => {
  await page.goto(catalogUrl("/catalog/php"));
  await expect(
    page.locator("[data-forge-cli]").filter({ hasText: "forge tool install" }),
  ).toHaveAttribute("data-forge-cli", "forge tool install php@8.4.27 --variant nts");
  await expect(page.getByTestId("plan-total")).toContainText("Total download");
  await page.getByRole("button", { name: "Install", exact: true }).click();
  const bar = page.getByRole("progressbar", { name: /Installing PHP 8.4.27/ });
  await expect(bar).toBeVisible();
  await expect(page.locator('[data-install-job][data-state="done"]')).toBeVisible();
  await expect(page.locator('[data-install-job] [data-part="check"]')).toBeVisible();
});

test("cancels an install in progress", async ({ page }) => {
  await page.goto(catalogUrl("/catalog/redis"));
  await page.getByRole("button", { name: "Install", exact: true }).click();
  await page.getByRole("button", { name: "Cancel" }).click();
  await expect(page.locator('[data-install-job][data-state="cancelled"]')).toBeVisible();
});

test("shows a banner with cached data when the providers are unreachable", async ({ page }) => {
  await page.goto(catalogUrl("/catalog/nodejs"));
  await expect(page.getByTestId("stale-banner")).toContainText("could not be reached");
  await expect(page.getByText("Series 22.11 (1 version)")).toBeVisible();
});

test("shows the cause and a retry when the releases fail", async ({ page }) => {
  await page.goto(catalogUrl("/catalog/legacy"));
  const notice = page.locator("[data-error-notice]");
  await expect(notice).toHaveAttribute("data-error-code", "network");
  await expect(notice.getByRole("button", { name: /try again|retry/i })).toBeVisible();
});

test("a stack shows the plan total before the install is confirmed", async ({ page }) => {
  await page.goto(catalogUrl("/catalog/laravel"));
  await expect(page.getByTestId("plan-total")).toContainText("≈");
  await expect(page.getByRole("button", { name: /Create project/ })).toBeDisabled();
  const confirm = page.getByRole("button", { name: /Confirm and install \(≈/ });
  await expect(confirm).toBeEnabled();
  await expect(page.locator("[data-install-job]")).toHaveCount(0);
  await confirm.click();
  await expect(page.locator("[data-install-job]")).toHaveCount(1);
});
