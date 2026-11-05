// CLS < 0.01 with the data arriving 300 ms late (latency=300 on every call of the scenario).
import { clsOf, expect, MAX_CLS, ROUTES, routeUrl, selftestUrl, test } from "./lib";

for (const route of ROUTES) {
  test(`CLS ${route}`, async ({ page }) => {
    const cls = await clsOf(page, routeUrl(route, { scenario: "populated", latency: 300 }));
    expect(cls).toBeLessThan(MAX_CLS);
  });
}

test.describe("negative self-tests", () => {
  test("the good page does not shift", async ({ page }) => {
    expect(await clsOf(page, selftestUrl("ok", { latency: 300 }))).toBeLessThan(MAX_CLS);
  });

  test("flags content pushed down by late data", async ({ page }) => {
    expect(await clsOf(page, selftestUrl("layout-shift", { latency: 300 }))).toBeGreaterThanOrEqual(
      MAX_CLS,
    );
  });
});
