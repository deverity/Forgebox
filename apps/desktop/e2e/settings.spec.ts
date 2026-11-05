// Settings on the qa build (M2-E3-T16, docs/07 section 5.11): schema-driven rows with save,
// the inline cause of config_invalid, the `forge config set` line, the shortcut editor
// (rebind, conflict, persistence across a reload), "Testar" hidden on remote, and accessibility.
// Shots land in qa/out/settings (gitignored).
import { expect, test, axeViolations } from "./fixtures";

const SHOT_DIR = "qa/out/settings";

function url(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/settings?${params.toString()}`;
}

test("Geral shows the root, the language and the GUI preferences", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await expect(page.getByTestId("root-path")).toHaveText(/\S/);
  await expect(page.getByTestId("setting-language")).toBeVisible();
  await expect(page.getByRole("switch", { name: "Close to tray" })).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/general-1280x800.png` });
  expect(await axeViolations(page)).toEqual([]);
});

test("a schema row saves, shows the forge command, and config_invalid reverts inline", async ({
  page,
}) => {
  await page.goto(url());
  await page.getByTestId("settings-nav-editor").click();
  const row = page.getByTestId("setting-editor.command");
  const input = row.getByRole("textbox", { name: "Editor command" });
  await input.fill("code -g {path}:{line}");
  await expect(row.locator("[data-forge-cli]")).toHaveAttribute(
    "data-forge-cli",
    'forge config set editor.command "code -g {path}:{line}"',
  );
  await row.getByRole("button", { name: "Save" }).click();
  await expect(row.getByTestId("setting-saved")).toBeVisible();
  await expect(input).toHaveValue("code -g {path}:{line}");

  await input.fill("BAD");
  await row.getByRole("button", { name: "Save" }).click();
  await expect(row.getByTestId("setting-error-cause")).toHaveText(
    "editor.command: unbalanced quotes",
  );
  await expect(input).toHaveValue("code -g {path}:{line}");
  await page.screenshot({ path: `${SHOT_DIR}/editor-error-1280x800.png` });
  expect(await axeViolations(page)).toEqual([]);
});

test("the shortcut editor rebinds, refuses a conflict and keeps the override after a reload", async ({
  page,
}) => {
  await page.goto(url());
  await page.getByTestId("settings-nav-shortcuts").click();
  const row = page.getByTestId("shortcut-sites.new");
  await row.getByTestId("shortcut-change-sites.new").click();
  await page.keyboard.press("Control+Shift+Y");
  await expect(row.getByTestId("shortcut-change-sites.new")).toContainText("Y");

  await row.getByTestId("shortcut-change-sites.new").click();
  await page.keyboard.press("Control+L");
  await expect(row.getByTestId("shortcut-error")).toContainText("already used by");

  await page.reload();
  await page.getByTestId("settings-nav-shortcuts").click();
  await expect(page.getByTestId("shortcut-change-sites.new")).toContainText("Y");
  await page.getByTestId("shortcut-reset-sites.new").click();
  await expect(page.getByTestId("shortcut-change-sites.new")).toContainText("N");
});

test("Ctrl+, opens the screen", async ({ page }) => {
  await page.goto("/dashboard?scenario=populated&locale=en-US");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.waitForTimeout(500);
  await page.keyboard.press("Control+Comma");
  await expect(page.getByTestId("page-settings")).toBeVisible();
});

test("Testar is offered locally and hidden on a remote daemon", async ({ page }) => {
  await page.goto(url());
  await page.getByTestId("settings-nav-editor").click();
  await expect(page.getByTestId("test-editor")).toBeVisible();
  await page.goto(url({ scenario: "remote" }));
  await page.getByTestId("settings-nav-editor").click();
  await expect(page.getByTestId("setting-editor.command")).toBeVisible();
  await expect(page.getByTestId("test-editor")).toHaveCount(0);
});

test("pt-BR dark keeps Editor/Terminal readable", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url({ locale: "pt-BR", theme: "dark", vibrancy: "off" }));
  await page.getByTestId("settings-nav-editor").click();
  await expect(page.getByRole("textbox", { name: "Comando do editor" })).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/editor-pt-dark.png` });
  expect(await axeViolations(page)).toEqual([]);
});
