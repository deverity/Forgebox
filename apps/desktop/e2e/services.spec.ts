// The Services screen on the qa build (M2-E2-T12): grouped list, a service.failed event that
// updates one row, the 420 px detail sheet, and screenshots at DPR 1 / 1.25 / 1.5.
// Shots land in qa/out/services (gitignored); qa:check covers /services in light, dark,
// vibrancy-off and reduced motion through ROUTES.
import type { QaHandle } from "../src/qa/bootstrap";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/services";

function servicesUrl(extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/services?${params.toString()}`;
}

test("groups the stack by category and keeps the layout fixed", async ({ page }) => {
  await page.goto(servicesUrl());
  const headings = page.locator("main h2");
  await expect(headings).toHaveText(["Databases", "Cache", "Mail"]);
  await expect(page.locator("[data-service-row]")).toHaveCount(3);
  const mysql = page.locator('[data-service-row][data-target="mysql/default"]');
  await expect(mysql.getByTestId("row-state")).toContainText("Running");
  await expect(mysql.getByRole("button", { name: "Stop" })).toBeVisible();
});

test("a service.failed event changes only that row", async ({ page }) => {
  await page.goto(servicesUrl());
  const redis = page.locator('[data-service-row][data-target="redis/default"]');
  const mysql = page.locator('[data-service-row][data-target="mysql/default"]');
  await expect(redis).toHaveAttribute("data-state", "stopped");
  const id = await redis.getAttribute("data-service-row");
  await page.evaluate((instanceId) => {
    const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
    qa.bridge.publish("service.failed", {
      v: 1,
      instance_id: instanceId,
      name: "redis/default",
      tool: "redis",
      state: "failed",
      pid: null,
      strategy: null,
      exit_code: null,
      error: { code: "port_in_use", message: "port 6379 is in use" },
      reason: { kind: "port_in_use", port: 6379, pid: 4321, exe: "redis-server.exe" },
    });
    qa.bridge.flush();
  }, id);
  await expect(redis).toHaveAttribute("data-state", "failed");
  await expect(mysql).toHaveAttribute("data-state", "running");
  await page.getByRole("button", { name: "Details of redis/default" }).click();
  const card = page.getByTestId("failure-card");
  await expect(card).toContainText("Port 6379 is in use by redis-server.exe (PID 4321).");
  await card.getByRole("button", { name: "Change port" }).click();
  await card.getByLabel("New main port").fill("6380");
  await expect(card.locator("[data-forge-cli]").first()).toHaveAttribute(
    "data-forge-cli",
    "forge service set redis/default ports.main=6380",
  );
});

for (const dpr of [1, 1.25, 1.5]) {
  test.describe(`DPR ${String(dpr)}`, () => {
    test.use({ deviceScaleFactor: dpr, viewport: { width: 1280, height: 800 } });

    for (const theme of ["light", "dark"] as const) {
      test(`list and 420 px sheet, ${theme}`, async ({ page }) => {
        await page.goto(servicesUrl({ theme, vibrancy: "off" }));
        await expect(page.locator("[data-service-row]")).toHaveCount(3);
        await page.screenshot({ path: `${SHOT_DIR}/dpr-${String(dpr)}-${theme}-list.png` });
        await page.getByRole("button", { name: "Details of mysql/default" }).click();
        const sheet = page.getByTestId("service-sheet");
        await expect(sheet).toBeVisible();
        await expect(page.getByRole("tab")).toHaveCount(3);
        const aside = page.locator("aside[data-motion]").last();
        const box = await aside.boundingBox();
        expect(box?.width).toBeCloseTo(420, 0);
        await page.waitForTimeout(400);
        await page.screenshot({ path: `${SHOT_DIR}/dpr-${String(dpr)}-${theme}-sheet.png` });
      });
    }
  });
}
