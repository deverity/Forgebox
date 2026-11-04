// Budget "interaction < 50 ms" (docs/07 section 10): no long task above 50 ms and no click
// handled slower than 50 ms while navigating every sidebar route and clicking around.
import {
  maxOf,
  NAV_ROUTES,
  navigateTo,
  openApp,
  PerfReport,
  perfUrl,
  readProbe,
  startProbe,
  test,
} from "./lib";

const BUDGET_MS = 50;

// Event Timing durations run until the next paint; with the 30 Hz vsync of an RDP session that
// alone is 33 ms. Uncapping the frame rate leaves the handler and render work being measured.
test.use({ launchOptions: { args: ["--disable-frame-rate-limit", "--disable-gpu-vsync"] } });

test("navigation and clicks stay under 50 ms", async ({ page }) => {
  const report = new PerfReport("interaction");
  await openApp(page, perfUrl("/dashboard", "populated"));

  // Cold pass: the first visit of each route loads and compiles its lazy chunk. Reported, not
  // budgeted (chunk loading is the cold-start budget of perf:startup, not an interaction).
  await startProbe(page);
  for (const route of NAV_ROUTES) await navigateTo(page, route.id, route.path);
  const cold = await readProbe(page);
  report.note("coldLongTasks", cold.longTasks);
  report.note("coldEventMaxMs", maxOf(cold.events));

  // Warm pass and clicks: every interaction counts.
  await startProbe(page);
  for (const route of NAV_ROUTES) await navigateTo(page, route.id, route.path);
  // Clicks: open a service's detail sheet and close it, then type in the sites search.
  await navigateTo(page, "services", "/services");
  await page.locator("[data-service-row] button").first().click();
  await page.keyboard.press("Escape");
  await navigateTo(page, "sites", "/sites");
  const search = page.getByRole("searchbox").or(page.getByRole("textbox")).first();
  await search.click();
  await search.fill("site4");

  const probe = await readProbe(page);
  report.record("longTaskMaxMs", maxOf(probe.longTasks), "ms", "max", BUDGET_MS);
  report.record("clickEventMaxMs", maxOf(probe.events), "ms", "max", BUDGET_MS);
  report.note("longTasks", probe.longTasks);
  report.note("routes", NAV_ROUTES.length);
  report.finish();
});
