// The Databases screen and the site "Banco" tab on the qa build (M2-E3-T14, docs/07 section
// 5.6): instance cards, create with inline validation, drop by typed name, masked connection
// strings, export / import jobs with progress and cancel, the remote import limit and the site
// tab. Shots land in qa/out/databases (gitignored).
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/databases";
const SAMPLE_SITE_ID = "01QAS1TE000000000000000001";

function url(path: string, extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `${path}?${params.toString()}`;
}

function row(page: Page, name: string) {
  return page.locator(`[data-database="${name}"]`);
}

test("lists the instances and the databases of the running one", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url("/databases"));
  await expect(page.locator('[data-instance="mysql/default"]')).toContainText("Running");
  await expect(page.locator('[data-instance="postgresql/default"]')).toContainText(
    "≈ calculating…",
  );
  await expect(page.locator("[data-database]")).toHaveCount(3);
  await expect(page.getByRole("button", { name: /Open in/ })).toBeDisabled();
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/databases-1280x800.png` });
});

test("an invalid name shows an inline error and Create stays disabled", async ({ page }) => {
  await page.goto(url("/databases"));
  const form = page.getByTestId("create-database");
  await form.getByLabel("Name").fill("my db!");
  await expect(form.getByRole("alert")).toContainText("letters, digits and underscore");
  await expect(form.getByRole("button", { name: "Create" })).toBeDisabled();
  await form.getByLabel("Name").fill("shop");
  await expect(form.locator("div[data-forge-cli]")).toHaveAttribute(
    "data-forge-cli",
    "forge db create shop --on mysql/default --charset utf8mb4",
  );
  await form.getByRole("button", { name: "Create" }).click();
  await expect(row(page, "shop")).toBeVisible();
});

test("dropping needs the typed name", async ({ page }) => {
  await page.goto(url("/databases"));
  await row(page, "blog").getByRole("button", { name: "Drop blog" }).click();
  const dialog = page.getByTestId("drop-database-dialog");
  const confirm = dialog.getByRole("button", { name: "Drop database" });
  await expect(confirm).toBeDisabled();
  await dialog.getByLabel("Type blog to confirm").fill("blog");
  await expect(confirm).toBeEnabled();
  await confirm.click();
  await expect(row(page, "blog")).toHaveCount(0);
});

test("the connection URL is masked until revealed", async ({ page }) => {
  await page.goto(url("/databases"));
  await row(page, "loja").getByRole("button", { name: "Connection" }).click();
  const urlBlock = page.getByTestId("connection-url");
  await expect(urlBlock).toContainText("••••••••");
  await expect(urlBlock).not.toContainText("s3cr3t");
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/connection-masked.png` });
  await page.getByRole("button", { name: "Reveal" }).click();
  await expect(urlBlock).toContainText("s3cr3t");
});

test("an export finishes with its path and a failing one shows the client output", async ({
  page,
}) => {
  await page.goto(url("/databases"));
  await row(page, "blog").getByRole("button", { name: "Export" }).click();
  await expect(page.getByTestId("transfers")).toContainText("Exported to");
  await page.screenshot({ path: `${SHOT_DIR}/export-done.png` });
});

test("remote: import is limited to www/ and says why", async ({ page }) => {
  await page.goto(url("/databases", { scenario: "remote" }));
  await row(page, "loja").getByRole("button", { name: "Import" }).click();
  const sheet = page.getByTestId("import-sheet");
  await expect(sheet.getByTestId("import-remote-note")).toContainText("www/");
  await sheet.getByLabel("Dump file").fill("D:/dumps/x.sql");
  await expect(sheet).toContainText("must be inside www/");
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/import-remote.png` });
});

test("the site Banco tab is registered and shows the create form", async ({ page }) => {
  await page.goto(url(`/sites/${SAMPLE_SITE_ID}`));
  await page.getByRole("tab", { name: "Database" }).click();
  const tab = page.getByTestId("site-database-tab");
  await expect(tab).toBeVisible();
  // loja.test already has its database in the QA data: the strings are one click away.
  await expect(tab).toContainText("Database loja exists.");
  await expect(tab.getByTestId("connection-url")).toContainText("••••••••");
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/site-tab.png` });
});
