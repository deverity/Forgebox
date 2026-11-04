// Eternal skeletons (R-08, docs/07 section 1.2 and section 11 items 2-3): in the scenarios that
// can leave a call without a good answer, nothing may stay a placeholder once the page settles.
import {
  eternalSkeletonViolations,
  expect,
  ready,
  ROUTES,
  routeUrl,
  selftestUrl,
  test,
} from "./lib";

const SCENARIOS_UNDER_TEST = ["errors", "not-implemented", "daemon-down"] as const;

/**
 * Screens that still leave a skeleton behind in a scenario, owned by the fix tasks of the report
 * (docs/design/qa/M2-E4-report.md section 2.3). An entry here is a debt, not an exemption: the
 * test below fails when an entry stops reproducing, so whoever fixes the screen has to delete
 * the line (the stale-entry guard), and a new offender outside the list fails the build.
 */
const KNOWN_OPEN: Record<string, string> = {};

for (const route of ROUTES) {
  for (const scenario of SCENARIOS_UNDER_TEST) {
    const key = `${route}|${scenario}`;
    test(`no eternal skeleton: ${route} scenario ${scenario}`, async ({ page }) => {
      await page.goto(routeUrl(route, { scenario }));
      await ready(page);
      const found = await eternalSkeletonViolations(page);
      if (key in KNOWN_OPEN) {
        expect(found, `${key} is fixed: remove it from KNOWN_OPEN`).not.toEqual([]);
      } else {
        expect(found).toEqual([]);
      }
    });
  }
}

test.describe("negative self-tests", () => {
  test("the good page has no skeleton", async ({ page }) => {
    await page.goto(selftestUrl("ok", { scenario: "errors" }));
    await ready(page);
    expect(await eternalSkeletonViolations(page, 200)).toEqual([]);
  });

  test("flags eternal-skeleton", async ({ page }) => {
    await page.goto(selftestUrl("eternal-skeleton", { scenario: "errors" }));
    await ready(page);
    expect((await eternalSkeletonViolations(page, 200)).join("\n")).toMatch(/skeleton/);
  });
});
