// Settings > Rede and Seguranca on the qa build (M2-E3-T17, docs/07 section 5.11): the DNS job with
// its UAC notice, the TLD confirmation (cancel calls nothing), the rotation modal with the job,
// the CA export through the (browser) download fallback of the native dialog, the exact `forge`
// commands and accessibility. Shots land in qa/out/settings-network-security (gitignored).
import { expect, test, axeViolations } from "./fixtures";

const SHOT_DIR = "qa/out/settings-network-security";

function url(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "network-security", locale: "en-US", ...extra });
  return `/settings?${params.toString()}`;
}

test("DNS enable shows the UAC notice and progress, then the NRPT state", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await page.getByTestId("settings-nav-network").click();
  const card = page.getByTestId("setting-dns");
  await expect(card.getByTestId("dns-status")).toContainText("Stopped");
  await expect(card.locator('[data-forge-cli="forge dns enable"]')).toBeVisible();
  await card.getByRole("button", { name: "Enable DNS" }).click();
  await expect(card.getByTestId("dns-enable-status-uac")).toContainText("UAC");
  await expect(card.getByRole("progressbar")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/dns-uac-1280x800.png` });
  await expect(card.getByTestId("dns-mode")).toContainText("NRPT rule");
  await card.getByRole("button", { name: "Test host" }).click();
  await expect(card.getByTestId("dns-test-resolved")).toContainText("127.0.0.1");
  await expect(card.getByTestId("dns-test-expected")).toContainText("127.0.0.1");
  await expect(card.getByText("forge dns test meusite.test")).toBeVisible();
  expect(await axeViolations(page)).toEqual([]);
});

test("a TLD change asks first and cancel keeps the old TLD", async ({ page }) => {
  await page.goto(url());
  await page.getByTestId("settings-nav-network").click();
  const row = page.getByTestId("setting-tld");
  const field = row.getByRole("textbox", { name: "Local TLD" });
  await field.fill(".dev");
  await row.getByRole("button", { name: "Save" }).click();
  const dialog = page.getByTestId("tld-dialog");
  await expect(dialog).toContainText("reissues all certificates");
  await page.screenshot({ path: `${SHOT_DIR}/tld-confirm-1280x800.png` });
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
  await expect(field).toHaveValue(".test");
});

test("Segurança: install hint, export, rotation modal with the job and uninstall", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await page.getByTestId("settings-nav-security").click();
  const card = page.getByTestId("setting-ca");
  await expect(card.getByTestId("ca-status")).toContainText("Forgebox Local CA");

  await card.getByRole("radio", { name: "Whole machine" }).click();
  await expect(card.getByText("forge tls ca install --scope machine")).toBeVisible();

  await expect(card.getByText("forge tls ca export -o ca.crt")).toBeVisible();
  const download = page.waitForEvent("download");
  await card.getByRole("button", { name: "Export CA…" }).click();
  expect((await download).suggestedFilename()).toBe("forgebox-ca.crt");

  await expect(card.getByText("forge tls ca uninstall", { exact: true })).toBeVisible();
  await card.getByTestId("ca-rotate").click();
  const modal = page.getByTestId("rotate-dialog");
  await expect(modal).toContainText("reissues every site certificate");
  await expect(modal.getByText("forge tls ca rotate")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/rotate-modal-1280x800.png` });
  await modal.getByRole("button", { name: "Rotate now" }).click();
  await expect(card.getByTestId("ca-rotate-status")).toHaveAttribute("data-phase", "running");
  await expect(card.getByTestId("ca-rotate-status")).toHaveAttribute("data-phase", "done");

  await card.getByTestId("ca-uninstall-user").click();
  await expect(card.getByTestId("ca-uninstall-user-status")).toHaveAttribute("data-phase", "done");
  await page.screenshot({ path: `${SHOT_DIR}/security-1280x800.png`, fullPage: true });
  expect(await axeViolations(page)).toEqual([]);
});

test("certificates renew and the helper log is shown", async ({ page }) => {
  await page.goto(url({ locale: "pt-BR" }));
  await page.getByTestId("settings-nav-security").click();
  const old = page.getByTestId("cert-velho.test");
  await expect(old).toHaveAttribute("data-state", "expired");
  await old.getByRole("button", { name: "Renovar" }).click();
  await expect(old).toHaveAttribute("data-state", "ok");
  await expect(page.getByTestId("helper-log")).toContainText("dns.nrpt_apply");
  expect(await axeViolations(page)).toEqual([]);
});
