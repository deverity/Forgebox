// The Site detail on the qa build (M2-E2-T16): the live PHP switch (progress, done after
// job.done + site.php_changed, failure with rollback, not installed -> confirm -> install ->
// retry), Processos following service events and Diagnostico with copyable fixes.
// Shots land in qa/out/site-detail.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/site-detail";
const SITE = "01QAS1TE000000000000000001";

function url(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/sites/${SITE}?${params.toString()}`;
}

async function pick(page: Page, series: string) {
  await page.getByRole("combobox", { name: "PHP version" }).click();
  await page.getByRole("option", { name: new RegExp(`^${series}`) }).click();
}

test("switching PHP shows progress and finishes only after the change", async ({ page }) => {
  await page.goto(url());
  const general = page.getByTestId("site-general");
  await expect(general).toBeVisible();
  await expect(page.getByRole("combobox", { name: "PHP version" })).toContainText("8.3");
  await pick(page, "8.2");
  const control = page.locator("[data-php-switch]");
  await expect(control).toHaveAttribute("data-php-switch", "switching");
  await expect(page.getByText("Switching to 8.2…")).toBeVisible();
  await expect(page.getByRole("combobox", { name: "PHP version" })).toBeDisabled();
  await page.screenshot({ path: `${SHOT_DIR}/php-switching.png` });
  await expect(page.getByText("PHP 8.2 is active")).toBeVisible();
  await expect(page.getByRole("combobox", { name: "PHP version" })).toBeEnabled();
  await page.screenshot({ path: `${SHOT_DIR}/php-done.png` });
});

test("a failed switch restores the old PHP and explains why", async ({ page }) => {
  await page.goto(url());
  await expect(page.getByTestId("site-general")).toBeVisible();
  // 7.4 is offered by the registry; install it first, then the pool fails to start.
  await pick(page, "7.4");
  await page.getByRole("button", { name: "Install and switch" }).click();
  const failure = page.locator("[data-php-switch-failure]");
  await expect(failure).toBeVisible({ timeout: 8000 });
  await expect(failure).toContainText("PHP stays at 8.3.");
  await expect(page.getByRole("combobox", { name: "PHP version" })).toContainText("8.3");
  await page.screenshot({ path: `${SHOT_DIR}/php-failed.png` });
});

test("a version that is not installed asks first, installs and retries", async ({ page }) => {
  await page.goto(url());
  await expect(page.getByTestId("site-general")).toBeVisible();
  await pick(page, "8.4");
  const dialog = page.getByTestId("php-install-confirm");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByTestId("php-install-size")).toContainText("32 MB");
  await expect(dialog).toContainText("forge tool install php@8.4");
  await page.screenshot({ path: `${SHOT_DIR}/php-install-confirm.png` });
  await dialog.getByRole("button", { name: "Keep the current PHP" }).click();
  await expect(page.getByRole("combobox", { name: "PHP version" })).toContainText("8.3");
  await pick(page, "8.4");
  await page.getByRole("button", { name: "Install and switch" }).click();
  await expect(page.getByText("PHP 8.4 is active")).toBeVisible({ timeout: 8000 });
});

test("Processes follow the service events", async ({ page }) => {
  await page.goto(url());
  await page.getByRole("tab", { name: "Processes" }).click();
  const queue = page.locator('[data-process-row="queue"]');
  await expect(queue).toHaveAttribute("data-state", "stopped");
  await queue.getByRole("button", { name: "Start", exact: true }).click();
  await expect(queue).toHaveAttribute("data-state", "running");
  await expect(queue.getByRole("button", { name: /Logs/ })).toBeDisabled();
  await page.screenshot({ path: `${SHOT_DIR}/processes.png` });
});

test("Diagnostics render the checks with copyable fixes and test DNS", async ({ page }) => {
  await page.goto(url());
  await page.getByRole("tab", { name: "Diagnostics" }).click();
  await expect(page.locator('[data-check="site.dns"]')).toHaveAttribute("data-status", "ok");
  await expect(page.locator('[data-check="site.root_access"]')).toHaveAttribute(
    "data-status",
    "warn",
  );
  const failed = page.locator('[data-check="site.php_pool"]');
  await expect(failed).toHaveAttribute("data-status", "fail");
  await expect(failed.getByTestId("check-fix")).toHaveText("forge start php");
  await expect(failed.getByRole("button", { name: "Copy fix" })).toBeVisible();
  await page.getByRole("button", { name: "Test DNS" }).click();
  await expect(page.getByTestId("dns-test").getByRole("status")).toContainText("resolves");
  await page.screenshot({ path: `${SHOT_DIR}/diagnostics.png` });
});
