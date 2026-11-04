// Settings > Sistema de arquivos, Registries and Atualizacoes on the qa build (M2-E3-T18,
// docs/07 section 5.11): the convert job with progress, the Case Mismatch Finder (dry run, then
// apply only after confirm), registries (badges, inline URL error, remove confirm, update job),
// quiet hours, the policy select with its exact `forge` command, and accessibility. Shots land in
// qa/out/settings-fs-registries-updates (gitignored).
import { expect, test, axeViolations } from "./fixtures";

const SHOT_DIR = "qa/out/settings-fs-registries-updates";

function url(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "filesystem", locale: "en-US", ...extra });
  return `/settings?${params.toString()}`;
}

test("the convert job shows progress and the table follows the result", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await page.getByTestId("settings-nav-filesystem").click();
  const table = page.getByTestId("setting-fs-table");
  await expect(table.getByTestId("case-row-data")).toHaveAttribute("data-state", "disabled");
  await expect(table.getByTestId("case-row-tmp")).toHaveAttribute("data-state", "unsupported");
  const card = page.getByTestId("setting-fs-convert");
  await card.getByRole("combobox", { name: "Folder to convert" }).click();
  await page.getByRole("option", { name: "data/" }).click();
  await card.getByTestId("fs-convert-run").click();
  await expect(card.getByTestId("fs-convert-status")).toHaveAttribute("data-phase", "running");
  await expect(card.getByRole("progressbar")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/convert-progress-1280x800.png` });
  await expect(card.getByTestId("fs-convert-report")).toContainText("1203 files");
  await expect(table.getByTestId("case-row-data")).toHaveAttribute("data-state", "enabled");
  await expect(
    card.locator('[data-forge-cli^="forge fs case-sensitive convert"]').first(),
  ).toBeVisible();
  expect(await axeViolations(page)).toEqual([]);
});

test("the finder lists file:line and applies only after the confirmation", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await page.getByTestId("settings-nav-filesystem").click();
  const card = page.getByTestId("setting-fs-finder");
  await card.getByTestId("finder-scan").click();
  const report = card.getByTestId("finder-report");
  await expect(report).toHaveAttribute("data-applied", "false");
  await expect(report.getByTestId("finding-open-app/index.php:12:9")).toContainText(
    "app/index.php:12",
  );
  await page.screenshot({ path: `${SHOT_DIR}/finder-dry-run-1280x800.png` });
  await card.getByTestId("finder-apply").click();
  const dialog = page.getByTestId("finder-dialog");
  await expect(dialog).toContainText("--strategy auto");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(report).toHaveAttribute("data-applied", "false");
  await card.getByTestId("finder-apply").click();
  await page.getByTestId("finder-dialog").getByRole("button", { name: "Apply" }).click();
  await expect(card.getByTestId("finder-report")).toHaveAttribute("data-applied", "true");
});

test("registries: badges, inline URL error, remove confirmation and the update job", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await page.getByTestId("settings-nav-registries").click();
  await expect(page.getByTestId("registry-official")).toHaveAttribute("data-trust", "official");
  await expect(page.getByTestId("registry-acme")).toHaveAttribute("data-trust", "third-party");
  await expect(page.getByTestId("registry-lab")).toHaveAttribute("data-trust", "unverified");

  const field = page.getByRole("textbox", { name: "Registry URL" });
  await field.fill("ftp://nope");
  await field.blur();
  await expect(page.getByText(/Use an https:\/\/ URL/).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Add registry" })).toBeDisabled();
  await page.screenshot({ path: `${SHOT_DIR}/registries-1280x800.png` });

  await page.getByTestId("registry-acme").getByRole("button", { name: "Remove" }).click();
  const dialog = page.getByTestId("registry-remove-dialog");
  await expect(dialog.getByText("forge registry remove acme")).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByTestId("registry-acme")).toBeVisible();

  await page.getByTestId("registry-update").click();
  await expect(page.getByTestId("registry-update-status")).toHaveAttribute("data-phase", "running");
  await expect(page.getByTestId("registry-update-status")).toHaveAttribute("data-phase", "done");
  expect(await axeViolations(page)).toEqual([]);
});

test("updates: disabled app channel, quiet hours validation and the policy command", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await page.getByTestId("settings-nav-updates").click();
  const app = page.getByTestId("update-app-inactive");
  await expect(app.getByRole("combobox", { name: "Channel" })).toBeDisabled();
  await expect(app).toContainText("arrive in 1.0");

  const quiet = page.getByTestId("setting-update-quiet");
  await quiet.getByRole("textbox", { name: "From" }).fill("25:00");
  await quiet.getByRole("textbox", { name: "Until" }).fill("07:00");
  await expect(quiet.getByText(/Use HH:MM/).first()).toBeVisible();
  await expect(quiet.getByRole("button", { name: "Save" })).toBeDisabled();
  await quiet.getByRole("textbox", { name: "From" }).fill("22:00");
  await expect(quiet.getByRole("button", { name: "Save" })).toBeEnabled();

  const row = page.getByTestId("policy-php");
  await expect(row.getByText("forge tool policy php notify")).toBeVisible();
  await row.getByRole("combobox", { name: "Update policy of PHP" }).click();
  await page.getByRole("option", { name: "Patch automatically" }).click();
  await expect(row.getByText("forge tool policy php auto-patch")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/updates-1280x800.png` });

  await page.getByTestId("update-check-now").click();
  await expect(page.getByTestId("update-check-result")).toContainText("1 tool has an update");
  expect(await axeViolations(page)).toEqual([]);
});
