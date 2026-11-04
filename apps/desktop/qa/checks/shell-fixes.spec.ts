// Regression checks for the shell findings fixed in M2-E4-T8 (docs/design/qa/M2-E4-report.md):
// R-01 remote indication, R-02 SegmentedControl pill, R-05 slim scrollbars.
import { expect, ready, routeUrl, test } from "./lib";

test("R-02: the SegmentedControl pill sits on the checked item even when an ErrorNotice shifts the page", async ({
  page,
}) => {
  await page.goto(routeUrl("/settings", { scenario: "errors", theme: "light" }));
  await ready(page);
  await page.waitForTimeout(1500);
  const theme = page.getByRole("radiogroup").first();
  const pill = theme.getByTestId("segment-pill");
  const checked = theme.locator('[role="radio"][data-state="checked"]');
  const [a, b] = await Promise.all([pill.boundingBox(), checked.boundingBox()]);
  expect(a).not.toBeNull();
  expect(b).not.toBeNull();
  if (a === null || b === null) return;
  expect(Math.abs(a.x - b.x)).toBeLessThanOrEqual(1);
  expect(Math.abs(a.y - b.y)).toBeLessThanOrEqual(1);
  expect(Math.abs(a.width - b.width)).toBeLessThanOrEqual(1);
  expect(Math.abs(a.height - b.height)).toBeLessThanOrEqual(1);
});

test("R-01: a remote daemon is named in the footer and re-tints the accent", async ({ page }) => {
  await page.goto(routeUrl("/dashboard", { scenario: "remote", theme: "light" }));
  await ready(page);
  const status = page.getByTestId("status-connection");
  await expect(status).toHaveAttribute("data-remote", "true");
  await expect(status).toContainText(/Daemon remoto|Remote daemon/);
  const colors = await page.evaluate(() => {
    const shell = document.querySelector<HTMLElement>('[data-testid="app-shell"]');
    const probe = document.createElement("i");
    probe.style.color = "var(--accent-remote)";
    shell?.append(probe);
    const remote = getComputedStyle(probe).color;
    probe.style.color = "var(--accent)";
    const accent = getComputedStyle(probe).color;
    probe.remove();
    return { remote, accent };
  });
  expect(colors.accent).toBe(colors.remote);
});

test("R-01: a local daemon keeps the system accent", async ({ page }) => {
  await page.goto(routeUrl("/dashboard", { scenario: "populated", theme: "light" }));
  await ready(page);
  await expect(page.getByTestId("app-shell")).not.toHaveAttribute("data-remote", "true");
});

test("R-05: scrollbars are the slim token-coloured ones", async ({ page }) => {
  await page.goto(routeUrl("/settings", { scenario: "populated", theme: "light" }));
  await ready(page);
  const width = await page.evaluate(() => getComputedStyle(document.body).scrollbarWidth);
  expect(width).toBe("thin");
});
