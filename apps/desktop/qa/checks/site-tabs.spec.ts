// The site tabs the E3 screens plug into the detail screen (M2-E3-T21): Database (T14),
// Environment (T20) and Processes (T12 logs button). The route table of lib.ts only opens the
// default tab, so these cases click the tab first, then run axe in both themes, the DOM
// contracts (every action has its `forge` command, errors have cause + action, remote rules)
// and the keyboard rule.
import {
  axeViolations,
  contractViolations,
  expect,
  keyboardViolations,
  ready,
  routeUrl,
  test,
  withMaterialBackdrop,
} from "./lib";

const SITE = "/sites/01QAS1TE000000000000000001";
// Scenarios whose data holds the sample site (the rest has no site to open).
const SITE_SCENARIOS = ["populated", "heavy", "slow", "remote"] as const;
const TABS = [
  { name: "Database", testId: "site-database-tab" },
  { name: "Environment", testId: "site-env-tab" },
  { name: "Processes", testId: undefined },
] as const;

for (const tab of TABS) {
  for (const theme of ["light", "dark"] as const) {
    for (const vibrancy of ["on", "off"] as const) {
      test(`axe: site tab ${tab.name} ${theme} vibrancy ${vibrancy}`, async ({ page }) => {
        await page.goto(
          routeUrl(SITE, { scenario: "populated", theme, vibrancy, locale: "en-US" }),
        );
        await ready(page);
        await page.getByRole("tab", { name: tab.name }).click();
        if (tab.testId !== undefined) await expect(page.getByTestId(tab.testId)).toBeVisible();
        if (vibrancy === "on") await withMaterialBackdrop(page);
        expect(await axeViolations(page)).toEqual([]);
        // Evidence for docs/design/qa/m2-e3-checklist.md (qa/out is gitignored).
        if (vibrancy === "off") {
          await page.screenshot({
            path: `qa/out/site-tabs/${tab.name.toLowerCase()}-${theme}.png`,
          });
        }
      });
    }
  }

  // Only the scenarios that have the sample site: the others (empty, errors, not-implemented,
  // daemon-down) never render the tab, so they are not listed instead of passing vacuously.
  for (const scenario of SITE_SCENARIOS) {
    test(`contracts: site tab ${tab.name} scenario ${scenario}`, async ({ page }) => {
      await page.goto(routeUrl(SITE, { scenario, locale: "en-US" }));
      await ready(page);
      await page.getByRole("tab", { name: tab.name }).click();
      await page.waitForTimeout(200);
      expect(await contractViolations(page, scenario === "remote")).toEqual([]);
    });
  }

  // The error state of the tab with the site present (item 3 of docs/07 section 11): the
  // tab's method fails, and the notice must carry the error code, a cause and an action.
  test(`contracts: site tab ${tab.name} method failing`, async ({ page }) => {
    const url = routeUrl(SITE, { locale: "en-US" }, { scenario: "tab-errors" });
    await page.goto(url);
    await ready(page);
    await page.getByRole("tab", { name: tab.name }).click();
    const notice = page.locator("[data-error-notice]");
    await expect(notice.first()).toBeVisible();
    expect(await notice.count()).toBeGreaterThan(0);
    expect(await contractViolations(page)).toEqual([]);
    expect(await axeViolations(page)).toEqual([]);
  });

  test(`keyboard: site tab ${tab.name}`, async ({ page }) => {
    await page.goto(routeUrl(SITE, { scenario: "populated", locale: "en-US" }));
    await ready(page);
    // Open the tab with the keyboard (Radix activates on Enter), then walk the page from the top.
    const trigger = page.getByRole("tab", { name: tab.name });
    await trigger.focus();
    await page.keyboard.press("Enter");
    await expect(trigger).toHaveAttribute("data-state", "active");
    expect(await keyboardViolations(page)).toEqual([]);
  });
}
