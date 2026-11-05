// Captures the README screenshots (M2-E4-T12) on the qa build with the populated fake-bridge
// scenario: no real paths, user names or hosts can appear, the data is the QA fixture.
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";
import {
  DPR,
  MAX_BYTES,
  MAX_WIDTH,
  SHOTS,
  VIEWPORT,
  imagePath,
  type ReadmeShot,
} from "./readme-shots";

const FIXED_TIME = new Date("2026-03-14T12:00:00.000Z");
const FREEZE_CSS =
  "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}";
/** `apps/desktop/qa` -> repo root. */
const REPO_ROOT = path.resolve(import.meta.dirname, "../../..");

async function settle(page: Page): Promise<void> {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          resolve();
        });
      });
    });
  });
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(500);
}

async function act(page: Page, shot: ReadmeShot): Promise<void> {
  if (shot.action === "palette") {
    await page.keyboard.press("Control+k");
    await page.locator("[role=dialog] [cmdk-input]").waitFor();
  } else if (shot.action === "site-performance") {
    const tab = page.getByRole("tab", { name: "Desempenho" });
    await tab.click();
    await expect(tab).toHaveAttribute("data-state", "active");
    await expect(page.locator("[data-chart-state='ready']").first()).toBeVisible();
    await expect(page.locator("[data-skeleton]")).toHaveCount(0);
  }
  await settle(page);
}

for (const shot of SHOTS) {
  test(`readme shot ${shot.file}`, async ({ browser }) => {
    const context = await browser.newContext({
      viewport: VIEWPORT,
      deviceScaleFactor: DPR,
      locale: "pt-BR",
      timezoneId: "UTC",
      colorScheme: shot.theme,
    });
    try {
      const page = await context.newPage();
      await page.clock.setFixedTime(FIXED_TIME);
      const params = new URLSearchParams({
        scenario: "populated",
        theme: shot.theme,
        vibrancy: "off",
        motion: "reduced",
        locale: "pt-BR",
      });
      await page.goto(`${shot.path}?${params.toString()}`);
      await page.addStyleTag({ content: FREEZE_CSS });
      await page.locator("#root > *").first().waitFor();
      await settle(page);
      await act(page, shot);
      const png = await page.screenshot({ animations: "disabled", caret: "hide", scale: "device" });
      expect(png.byteLength, `${shot.file} size`).toBeLessThanOrEqual(MAX_BYTES);
      expect(png.readUInt32BE(16), `${shot.file} width`).toBeLessThanOrEqual(MAX_WIDTH);
      const file = path.join(REPO_ROOT, imagePath(shot));
      mkdirSync(path.dirname(file), { recursive: true });
      writeFileSync(file, png);
    } finally {
      await context.close();
    }
  });
}
