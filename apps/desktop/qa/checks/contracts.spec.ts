// DOM contracts: every action control has data-forge-cli, every error state has data-error-code,
// cause text and an action, and on a remote context local_only actions are disabled with an
// explanation and no client C:\ path is shown.
import {
  contractViolations,
  expect,
  ready,
  ROUTES,
  routeUrl,
  SCENARIOS,
  selftestUrl,
  test,
} from "./lib";

for (const route of ROUTES) {
  for (const scenario of SCENARIOS) {
    test(`contracts: ${route} scenario ${scenario}`, async ({ page }) => {
      await page.goto(routeUrl(route, { scenario }));
      await ready(page);
      expect(await contractViolations(page, scenario === "remote")).toEqual([]);
    });
  }
}

test.describe("negative self-tests", () => {
  test("the good page passes, local and remote", async ({ page }) => {
    await page.goto(selftestUrl("ok", { scenario: "populated" }));
    await ready(page);
    expect(await contractViolations(page)).toEqual([]);
    await page.goto(selftestUrl("ok", { scenario: "remote" }));
    await ready(page);
    // The local_only action is really there, disabled, with its reason.
    await expect(page.locator('[data-local-only="true"]')).toHaveAttribute(
      "data-disabled-reason",
      /.+/,
    );
    expect(await contractViolations(page, true)).toEqual([]);
  });

  const NEGATIVE = [
    ["contract-cli", false, /no data-forge-cli/],
    ["contract-error", false, /data-error-code/],
    ["contract-local-only", true, /local_only action .* (has no explanation|is enabled)/],
    ["contract-client-path", true, /client path/],
  ] as const;
  for (const [which, remote, pattern] of NEGATIVE) {
    test(`flags ${which}`, async ({ page }) => {
      await page.goto(selftestUrl(which, { scenario: remote ? "remote" : "populated" }));
      await ready(page);
      expect((await contractViolations(page, remote)).join("\n")).toMatch(pattern);
    });
  }
});
