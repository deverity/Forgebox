// /sites in a real browser against the fake bridge (M2-E2-T15): the performance budget of
// 500 sites, the create/remove flows and the remote restrictions.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const CARDS = "[data-site-card]";

/** Ctrl+N on the loaded screen (the shortcut host mounts after the first paint, so retry). */
async function openCreateSheet(page: Page) {
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  const dialog = page.getByRole("dialog");
  await expect(async () => {
    await page.keyboard.press("Control+n");
    await expect(dialog).toBeVisible({ timeout: 1000 });
  }).toPass({ timeout: 10_000 });
  return dialog;
}

test("500 sites: a keystroke filters in under 50 ms and fewer than 60 cards are mounted", async ({
  page,
}) => {
  await page.goto("/sites?scenario=heavy");
  await expect(page.locator(CARDS).first()).toBeVisible();
  expect(await page.locator(CARDS).count()).toBeLessThan(60);

  const search = page.getByRole("searchbox", { name: /search sites|buscar sites/i });
  // The time from the input event until the next frame is painted with the filtered list.
  const measure = (value: string) =>
    search.evaluate(async (input: HTMLInputElement, text: string) => {
      // eslint-disable-next-line @typescript-eslint/unbound-method -- the native setter is called with .call(input), React tracks the value through it
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      const start = performance.now();
      setter?.call(input, text);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          resolve();
        });
      });
      return {
        ms: performance.now() - start,
        cards: document.querySelectorAll("[data-site-card]").length,
      };
    }, value);

  // Warm-up: the first swaps of the visible cards pay for JIT and first-time styles.
  await measure("s");
  await measure("site9");
  await measure("site8");
  const timings: number[] = [];
  for (const query of [
    "si",
    "site4",
    "site42",
    "site3",
    "site1",
    "site4",
    "site2",
    "site",
    "site49",
    "site",
  ]) {
    const result = await measure(query);
    timings.push(result.ms);
    expect(result.cards).toBeLessThan(60);
  }
  // One frame is 16.7 ms; the budget covers filtering, render and paint. A shared build machine
  // adds rare scheduler stalls (hundreds of ms on a single sample), so the gate is the median plus
  // "at least 8 of 10 keystrokes under budget" instead of the single worst sample.
  const sorted = [...timings].sort((x, y) => x - y);
  const median = sorted[Math.floor(sorted.length / 2)] ?? Infinity;
  const within = timings.filter((ms) => ms < 50).length;
  expect(median, timings.join(", ")).toBeLessThan(50);
  expect(within, timings.join(", ")).toBeGreaterThanOrEqual(8);

  await search.fill("site42");
  await expect(page.locator(CARDS).first()).toContainText("site42.test");
});

test("grid and list layouts persist across reloads", async ({ page }) => {
  await page.goto("/sites?scenario=populated");
  await expect(page.locator(CARDS)).toHaveCount(3);
  await page.getByRole("radio", { name: /list|lista/i }).click();
  await expect(page.locator(`${CARDS}[data-layout="list"]`)).toHaveCount(3);
  await page.reload();
  await expect(page.locator(`${CARDS}[data-layout="list"]`)).toHaveCount(3);
});

test("a host typed in uppercase becomes lowercase with a note; a bad TLD is an inline error", async ({
  page,
}) => {
  await page.goto("/sites?scenario=populated&locale=en-US");
  const dialog = await openCreateSheet(page);
  await dialog.getByLabel("Project folder").fill("C:\\www\\loja2");
  const host = dialog.getByLabel("Host");
  await host.fill("Loja2.Test");
  await expect(host).toHaveValue("loja2.test");
  await expect(dialog.getByText(/always lowercase/)).toBeVisible();

  await host.fill("loja2.invalid");
  await dialog.getByRole("button", { name: "Create site" }).click();
  await expect(host).toHaveAttribute("aria-invalid", "true");
  await expect(dialog.getByRole("alert")).toBeVisible();
  await expect(dialog).toBeVisible();

  await host.fill("loja2.test");
  await dialog.getByRole("button", { name: "Create site" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator(CARDS).filter({ hasText: "loja2.test" })).toHaveCount(1);
});

test("removing a site asks for confirmation and drops its card", async ({ page }) => {
  await page.goto("/sites?scenario=populated&locale=en-US");
  await expect(page.locator(CARDS)).toHaveCount(3);
  await page.getByRole("button", { name: "Remove blog.test" }).click();
  const dialog = page.getByTestId("remove-site-dialog");
  await expect(dialog).toContainText(/project files are never deleted/);
  await dialog.getByRole("button", { name: "Remove site" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator(CARDS).filter({ hasText: "blog.test" })).toHaveCount(0);
  await expect(page.locator(CARDS)).toHaveCount(2);
});

test("remote daemon: drag and drop and Browse are disabled with a reason", async ({ page }) => {
  await page.goto("/sites?scenario=remote&locale=en-US");
  const dialog = await openCreateSheet(page);
  const zone = dialog.getByTestId("drop-zone");
  await expect(zone).toHaveAttribute("aria-disabled", "true");
  await expect(zone).toContainText(/another machine/);
  const browse = dialog.getByRole("button", { name: "Browse…" });
  await expect(browse).toBeDisabled();
  await expect(browse).toHaveAttribute("data-disabled-reason", /another machine/);
});

test("proxy tab shows the equivalent forge command", async ({ page }) => {
  await page.goto("/sites?scenario=populated&locale=en-US");
  const dialog = await openCreateSheet(page);
  await dialog.getByRole("tab", { name: "Proxy" }).click();
  await dialog.getByLabel("Host").fill("app.test");
  await dialog.getByLabel("Upstream").fill("http://127.0.0.1:3000");
  await dialog.getByRole("switch", { name: /WebSocket/ }).click();
  await expect(dialog.locator("[data-forge-cli]")).toHaveAttribute(
    "data-forge-cli",
    "forge proxy add app.test http://127.0.0.1:3000 --ws --secure",
  );
  await expect(dialog.getByRole("tab", { name: "New project" })).toBeDisabled();
});
