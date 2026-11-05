// The Services detail tabs Configuracao, Versoes and Avancado on the qa build (M2-E2-T13):
// the quick-settings form with its CLI and port conflict, the remote fs.read viewer with the
// managed blocks highlighted, and the Versions rules. Shots land in qa/out/services-config.
import type { Page } from "@playwright/test";
import type { QaHandle } from "../src/qa/bootstrap";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/services-config";

function url(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/services?${params.toString()}`;
}

async function openTab(page: Page, tab: string, extra: Record<string, string> = {}) {
  await page.goto(url(extra));
  await page.locator('[data-service-row][data-target="mysql/default"] button').first().click();
  await expect(page.getByTestId("service-sheet")).toBeVisible();
  await page.getByRole("tab", { name: tab }).click();
}

test("the form shows the CLI of a change and refuses an invalid value or a reserved port", async ({
  page,
}) => {
  await openTab(page, "Configuration");
  const form = page.getByTestId("quick-settings");
  await expect(form.locator("[data-kind]")).toHaveCount(9);
  const save = form.locator('[data-action="services.set_settings"]');
  await expect(save).toHaveAttribute("data-disabled-reason", "Nothing changed yet.");

  await form.getByRole("textbox", { name: "Max connections" }).fill("5");
  await expect(form.getByText("Use a value between 10 and 1000.")).toBeVisible();
  await expect(save).toHaveAttribute("data-disabled-reason", "Fix the highlighted fields first.");
  await form.getByRole("textbox", { name: "Max connections" }).fill("200");
  await expect(save).toHaveAttribute(
    "data-forge-cli",
    "forge service set mysql/default max_connections=200",
  );
  await expect(form.getByTestId("apply-note")).toContainText("restarts the service");

  const port = form.getByRole("textbox", { name: "ports.main" });
  await port.fill("6379");
  await expect(form.locator('[data-setting="ports.main"]')).toHaveAttribute(
    "data-conflict",
    "true",
  );
  await expect(form.getByText("Port already in use by redis/default.").first()).toBeVisible();
  await expect(save).toHaveAttribute(
    "data-disabled-reason",
    "Port already in use by redis/default.",
  );
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/config-conflict.png` });

  await port.fill("3310");
  await expect(save).not.toHaveAttribute("data-disabled-reason", /.+/);
  await save.getByRole("button", { name: "Apply changes" }).click();
  await expect(save).toHaveAttribute("data-phase", "done");
});

test("remote: Open file becomes the fs.read viewer with the managed block highlighted", async ({
  page,
}) => {
  await openTab(page, "Configuration", { scenario: "remote" });
  // The remote scenario is an operator; fs.read needs admin (docs/13 section 3.3).
  await page.evaluate(() => {
    const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
    qa.bridge.setHello({ role: "admin" });
  });
  await expect(page.locator('[data-action="services.open_config"]')).toHaveCount(0);
  await page.getByRole("button", { name: "Open file: main" }).click();
  const viewer = page.getByTestId("file-viewer");
  await expect(viewer.locator("[data-managed]")).toHaveCount(4);
  await expect(viewer.locator("pre > div").nth(2)).toHaveAttribute("data-managed", "true");
  await expect(viewer.locator("pre > div").nth(5)).not.toHaveAttribute("data-managed", /.*/);
  await page.screenshot({ path: `${SHOT_DIR}/config-remote-viewer.png` });
});

test("versions: the version in use cannot be removed and the CLI commands are exact", async ({
  page,
}) => {
  await openTab(page, "Versions");
  const inUse = page.locator('[data-version="8.4.3"]');
  await expect(inUse).toHaveAttribute("data-in-use", "instance");
  await expect(inUse.getByRole("button", { name: "Remove 8.4.3" })).toBeDisabled();
  await expect(inUse.locator('[data-disabled-reason="In use by mysql/default."]')).toHaveCount(1);
  const free = page.locator('[data-version="8.0.40"]');
  await expect(free.locator('[data-action="tools.use_version"]')).toHaveAttribute(
    "data-forge-cli",
    "forge use mysql 8.0",
  );
  await free.getByRole("button", { name: "Remove 8.0.40" }).click();
  await expect(
    page.getByTestId("remove-version-confirm").locator('[data-action="tools.remove_version"]'),
  ).toHaveAttribute("data-forge-cli", "forge tool remove mysql@8.0.40");
  await expect(page.locator('[data-action="tools.rollback_series"]')).toHaveAttribute(
    "data-forge-cli",
    "forge tool rollback mysql@8.4",
  );
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/versions.png` });
});

test("advanced: the form lists what the daemon sent", async ({ page }) => {
  await openTab(page, "Advanced");
  const form = page.getByTestId("advanced-form");
  await expect(form.getByRole("textbox", { name: "Extra arguments" })).toBeVisible();
  const save = form.locator('[data-action="services.update_instance"]');
  await form.getByRole("switch", { name: "Start with Forgebox" }).click();
  await expect(save).toHaveAttribute(
    "data-forge-cli",
    "forge service update mysql/default --autostart",
  );
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/advanced.png` });
});
