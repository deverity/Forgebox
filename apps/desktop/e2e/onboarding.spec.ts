// The onboarding wizard on the qa build (M2-E2-T19, docs/07 section 5.1): the Laravel path with
// the keyboard only, a refused UAC, a failed download and its retry, a remote and a portable
// daemon, resuming after a reload, and axe on every step. Shots land in qa/out/onboarding.
import type { Locator, Page } from "@playwright/test";
import { axeViolations, expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/onboarding";

function url(scenario: string, extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario, locale: "en-US", ...extra });
  return `/onboarding?${params.toString()}`;
}

/** Tabs until `target` has the focus; fails when it never does. */
async function tabTo(page: Page, target: Locator, max = 60): Promise<void> {
  for (let i = 0; i < max; i += 1) {
    await page.keyboard.press("Tab");
    const focused = await target.evaluate((el) => el === document.activeElement);
    if (focused) return;
  }
  throw new Error("Tab never reached the target");
}

/** Clicks the card like a pointer user does (the radio itself is visually hidden). */
async function pickStack(page: Page, name: string): Promise<void> {
  await page.locator("label[data-stack-card]").filter({ hasText: name }).click();
}

const step = (page: Page, id: string) => page.locator(`[data-run-step="${id}"]`);

async function callCount(page: Page, method: string): Promise<number> {
  return page.evaluate(
    (m) => window.__FORGEBOX_QA__?.bridge.calls.filter((call) => call.method === m).length ?? 0,
    method,
  );
}

test("the Laravel path completes with the keyboard only, in order", async ({ page }) => {
  await page.goto(url("onboarding"));
  const root = page.getByRole("heading", { level: 2, name: "Where should Forgebox live?" });
  await expect(root).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/1-root.png` });

  await tabTo(page, page.getByRole("button", { name: "Continue" }));
  await page.keyboard.press("Enter");
  const stack = page.getByRole("heading", { level: 2, name: "What will you build?" });
  await expect(stack).toBeVisible();
  // Focus moved to the new step's heading, so the next Tab lands inside the step.
  await expect(stack).toBeFocused();

  // Laravel is the first card in index order: Tab reaches it, Space picks it.
  await tabTo(page, page.getByRole("radio", { name: /Laravel/ }));
  await page.keyboard.press("Space");
  await expect(page.getByTestId("plan-total")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/2-stack.png` });

  await tabTo(page, page.getByRole("button", { name: "Continue" }));
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("heading", { level: 2, name: "Permissions and parity" }),
  ).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/3-options.png` });

  await tabTo(page, page.getByRole("button", { name: "Install and set up" }));
  await page.keyboard.press("Enter");
  await expect(step(page, "ca")).toHaveAttribute("data-status", "done");
  for (const id of ["install", "path", "dns"]) {
    await expect(step(page, id)).toHaveAttribute("data-status", "done");
  }
  await expect(page.locator("[data-install-job]")).toHaveCount(3);
  await expect(page.getByTestId("first-site")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/4-run.png` });

  // The order the daemon saw them.
  const methods = await page.evaluate(
    () => window.__FORGEBOX_QA__?.bridge.calls.map((call) => call.method) ?? [],
  );
  const wanted = [
    "sites.park",
    "tools.plan",
    "tools.install",
    "shell.path_add",
    "net.dns_enable",
    "tls.ca_install",
  ];
  const first = wanted.map((method) => methods.indexOf(method));
  expect(first.every((index) => index >= 0)).toBe(true);
  expect([...first].sort((a, b) => a - b)).toEqual(first);

  await tabTo(page, page.getByRole("button", { name: "Finish" }));
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/dashboard/);
});

test("a refused UAC is skipped with its command and the CA still runs", async ({ page }) => {
  await page.goto(url("onboarding-uac"));
  await page.getByRole("button", { name: "Continue" }).click();
  await pickStack(page, "Laravel");
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByRole("button", { name: "Install and set up" }).click();
  await expect(step(page, "ca")).toHaveAttribute("data-status", "done");
  const dns = step(page, "dns");
  await expect(dns).toHaveAttribute("data-status", "skipped");
  await expect(dns).toHaveAttribute("data-skip", "elevation");
  await expect(dns.locator('[data-forge-cli="forge dns enable"]')).toBeVisible();
  await expect(dns.getByRole("button", { name: "Try again" })).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/run-uac.png` });
});

test("a failed download shows its cause and the retry installs only what is missing", async ({
  page,
}) => {
  await page.goto(url("onboarding-fail"));
  await page.getByRole("button", { name: "Continue" }).click();
  await pickStack(page, "Laravel");
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByRole("button", { name: "Install and set up" }).click();
  const install = step(page, "install");
  await expect(install).toHaveAttribute("data-status", "failed");
  await expect(install.locator('[data-error-code="verification_failed"]').first()).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/run-failed.png` });
  const before = await callCount(page, "tools.install");
  await install.getByRole("button", { name: "Try again" }).first().click();
  await expect(install).toHaveAttribute("data-status", "done");
  expect((await callCount(page, "tools.install")) - before).toBe(1);
  await expect(install.locator("[data-install-job]")).toHaveCount(3);
  await expect(install.locator("[data-error-code]")).toHaveCount(0);
});

test("a remote daemon hides step 1 and the PATH and startup toggles, with the reason", async ({
  page,
}) => {
  await page.goto(url("remote"));
  await expect(page.getByTestId("onboarding-hidden-note")).toHaveAttribute("data-reason", "remote");
  await expect(page.getByRole("heading", { level: 2, name: "What will you build?" })).toBeVisible();
  await pickStack(page, "Empty");
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("switch", { name: "Trust the local CA" })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Add Forgebox to PATH" })).toHaveCount(0);
  await expect(page.getByRole("switch", { name: "Start with Windows" })).toHaveCount(0);
});

test("portable mode hides step 1 and the PATH and startup toggles", async ({ page }) => {
  await page.goto(url("onboarding", { portable: "1" }));
  await expect(page.getByTestId("onboarding-hidden-note")).toHaveAttribute(
    "data-reason",
    "portable",
  );
  await pickStack(page, "Empty");
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("switch", { name: "Add Forgebox to PATH" })).toHaveCount(0);
  await expect(page.getByRole("switch", { name: "Start with Windows" })).toHaveCount(0);
});

test("closing at step 3 and opening again resumes with the previous choices", async ({ page }) => {
  await page.goto(url("onboarding"));
  await page.getByRole("button", { name: "Continue" }).click();
  await pickStack(page, "WordPress");
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByRole("switch", { name: /Automatic DNS/ }).click();
  await page.getByRole("switch", { name: "Start with Windows" }).click();
  // The window closes and opens again: only the stored progress survives.
  await page.reload();
  await expect(
    page.getByRole("heading", { level: 2, name: "Permissions and parity" }),
  ).toBeVisible();
  await expect(page.getByRole("switch", { name: /Automatic DNS/ })).not.toBeChecked();
  await expect(page.getByRole("switch", { name: "Start with Windows" })).toBeChecked();
  await page.getByRole("button", { name: "Back" }).click();
  await expect(page.getByRole("radio", { name: /WordPress/ })).toBeChecked();
});

test("every step passes axe in light and dark", async ({ page }) => {
  for (const theme of ["light", "dark"] as const) {
    await page.goto(url("onboarding", { theme }));
    await expect(
      page.getByRole("heading", { level: 2, name: "Where should Forgebox live?" }),
    ).toBeVisible();
    expect(await axeViolations(page), `root ${theme}`).toEqual([]);
    await page.getByRole("button", { name: "Continue" }).click();
    await pickStack(page, "Laravel");
    await expect(page.getByTestId("plan-total")).toBeVisible();
    expect(await axeViolations(page), `stack ${theme}`).toEqual([]);
    await page.getByRole("button", { name: "Continue" }).click();
    await expect(page.getByRole("switch", { name: /Linux parity mode/ })).toBeVisible();
    expect(await axeViolations(page), `options ${theme}`).toEqual([]);
    await page.getByRole("button", { name: "Install and set up" }).click();
    await expect(step(page, "ca")).toHaveAttribute("data-status", "done");
    expect(await axeViolations(page), `run ${theme}`).toEqual([]);
    await page.evaluate(() => {
      sessionStorage.clear();
    });
  }
});
