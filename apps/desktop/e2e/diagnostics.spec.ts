// The Diagnostics screen on the qa build (M2-E3-T19, docs/07 section 5.12): run with progress,
// grouped results, Fix (dns) with the row updating, Encerrar with its confirmation, copy-only
// fixes, hidden Encerrar on remote and the exported report (a download outside Tauri).
// Shots land in qa/out/diagnostics (gitignored).
import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/diagnostics";

function url(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/diagnostics?${params.toString()}`;
}

function row(page: Page, id: string) {
  return page.locator(`[data-check="${id}"]`);
}

async function run(page: Page) {
  await page.getByRole("button", { name: "Run diagnostics" }).click();
  await expect(page.getByTestId("doctor-summary")).toBeVisible();
}

test("idle, running with progress text, then grouped results", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url());
  await expect(page.getByText("Nothing checked yet")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/idle-1280x800.png` });
  await page.getByRole("button", { name: "Run diagnostics" }).click();
  await expect(page.getByTestId("doctor-progress")).toContainText("Checking:");
  await expect(page.getByTestId("doctor-summary")).toContainText("4 ok · 4 warnings · 1 failures");
  await expect(page.locator("[data-group]")).toHaveCount(8);
  await expect(row(page, "root.writable")).toBeInViewport();
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/done-1280x800.png` });
});

test("dark theme keeps the rows readable", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(url({ theme: "dark", vibrancy: "off" }));
  await run(page);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/done-dark.png` });
});

test("Fix on the DNS policy updates the row and leaves the others", async ({ page }) => {
  await page.goto(url());
  await run(page);
  await row(page, "dns.policy.test").getByRole("button", { name: "Fix" }).click();
  await expect(row(page, "dns.policy.test")).toHaveAttribute("data-status", "ok");
  await expect(row(page, "path.user")).toHaveAttribute("data-status", "warn");
});

test("a fix outside the allowlist only offers the command", async ({ page }) => {
  await page.goto(url());
  await run(page);
  const nginx = row(page, "service.nginx");
  await expect(nginx.getByRole("button", { name: "Fix" })).toHaveCount(0);
  await expect(nginx.locator("div[data-forge-cli]")).toHaveAttribute(
    "data-forge-cli",
    "forge daemon stop",
  );
});

test("Encerrar confirms with the pid and exe and frees the port", async ({ page }) => {
  await page.goto(url());
  await run(page);
  await row(page, "port.80").getByRole("button", { name: "End process" }).click();
  const dialog = page.getByTestId("kill-port-dialog");
  await expect(dialog.getByTestId("kill-holder")).toContainText("4412");
  await expect(dialog.getByTestId("kill-holder")).toContainText("httpd.exe");
  await page.screenshot({ path: `${SHOT_DIR}/kill-dialog.png` });
  await dialog.getByRole("button", { name: "End process" }).click();
  await expect(row(page, "port.80")).toHaveAttribute("data-status", "ok");
});

test("Encerrar is not offered on a remote daemon", async ({ page }) => {
  await page.goto(url({ scenario: "remote" }));
  await run(page);
  await expect(row(page, "port.80").getByRole("button", { name: "End process" })).toHaveCount(0);
  await expect(row(page, "port.80").getByRole("button", { name: "Change port" })).toBeVisible();
});

test("Exportar relatorio downloads the bundle of forge doctor --export", async ({ page }) => {
  await page.goto(url());
  await run(page);
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export report" }).click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^forgebox-diagnostics-\d{8}-\d{6}\.json$/);
  const path = await file.path();
  const bundle = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
  expect(Object.keys(bundle)).toEqual([
    "v",
    "created_unix",
    "forge_version",
    "api_version",
    "doctor",
    "status",
    "tools",
    "daemon_log",
  ]);
  expect((bundle.doctor as { checks: unknown[] }).checks).toHaveLength(10);
});

test("pt-BR labels", async ({ page }) => {
  await page.goto(url({ locale: "pt-BR" }));
  await expect(page.getByRole("button", { name: "Executar diagnóstico" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Testar DNS" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Exportar relatório" })).toBeVisible();
});
