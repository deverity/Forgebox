// Budgets "slow daemon" and "daemon down" (docs/03 section 7.5, docs/07 section 10):
// - every call answers 1.5 s late (scenario `slow`) and navigation still takes < 100 ms;
// - the daemon dies: the banner says "reconnecting" within 1 s, navigation keeps working, and
//   after the reconnect the Activity feed input has no duplicate event ids.
import {
  expect,
  medianOf,
  NAV_ROUTES,
  navigateTo,
  openApp,
  PerfReport,
  perfUrl,
  test,
} from "./lib";

const NAV_BUDGET_MS = 100;
const RECONNECTING_BUDGET_MS = 1000;

test("slow daemon: navigation under 100 ms", async ({ page }) => {
  const report = new PerfReport("resilience-slow");
  await openApp(page, perfUrl("/dashboard", "slow"));
  // The first visit of a route also loads its lazy chunk (cold-start budget, not this one).
  for (const route of NAV_ROUTES) await navigateTo(page, route.id, route.path);
  // Three warm passes. The budget is the median: on a shared machine a single visit can stall
  // for a frame or two for reasons outside the app (the maximum is reported as a note).
  const times: number[] = [];
  for (let pass = 0; pass < 3; pass += 1) {
    for (const route of NAV_ROUTES) times.push(await navigateTo(page, route.id, route.path));
  }
  report.record("navigationMedianMs", medianOf(times), "ms", "max", NAV_BUDGET_MS);
  report.note("navigationMaxMs", Math.max(...times));
  report.note("navigationMs", times);
  report.finish();
});

test("daemon down: reconnecting within 1 s, navigation works, no duplicate events", async ({
  page,
}) => {
  const report = new PerfReport("resilience-down");
  await openApp(page, perfUrl("/services", "populated"));
  await page.waitForSelector("[data-service-row]");

  const publish = (count: number) =>
    page.evaluate((n) => {
      const qa = window.__FORGEBOX_QA__;
      const inst = qa?.data.instances[0];
      if (qa === undefined || inst === undefined) throw new Error("no qa handle");
      const seqs: number[] = [];
      for (let i = 0; i < n; i += 1) {
        seqs.push(
          qa.bridge.publish("service.running", {
            v: 1,
            instance_id: inst.id,
            name: `${inst.tool}/${inst.name}`,
            tool: inst.tool,
            state: "running",
            pid: 2000,
            strategy: null,
            exit_code: null,
            error: null,
          }),
        );
      }
      return seqs;
    }, count);

  for (const route of NAV_ROUTES) await navigateTo(page, route.id, route.path);
  await navigateTo(page, "services", "/services");
  await page.evaluate(() => {
    window.__FORGEBOX_RENDERS__?.reset();
  });
  const before = await publish(20);
  await page.waitForFunction((n) => (window.__FORGEBOX_RENDERS__?.ledger().length ?? 0) >= n, 20);

  // The daemon dies: time until the banner shows "reconnecting".
  const reconnectingMs = await page.evaluate(
    () =>
      new Promise<number>((resolve, reject) => {
        const started = performance.now();
        const poll = () => {
          const banner = document.querySelector('[data-testid="connection-banner"]');
          if (banner?.getAttribute("data-status") === "reconnecting") {
            resolve(performance.now() - started);
            return;
          }
          if (performance.now() - started > 5000) reject(new Error("no reconnecting banner"));
          else requestAnimationFrame(poll);
        };
        window.__FORGEBOX_QA__?.bridge.crash();
        poll();
      }),
  );
  report.record("reconnectingBannerMs", reconnectingMs, "ms", "max", RECONNECTING_BUDGET_MS);

  // Still navigable while down.
  const navTimes: number[] = [];
  for (let pass = 0; pass < 3; pass += 1) {
    for (const route of NAV_ROUTES) navTimes.push(await navigateTo(page, route.id, route.path));
  }
  report.record("navigationWhileDownMedianMs", medianOf(navTimes), "ms", "max", NAV_BUDGET_MS);
  report.note("navigationWhileDownMaxMs", Math.max(...navTimes));

  // Events published while the connection is down are replayed from the cursor on reconnect.
  const during = await publish(30);
  await page.evaluate(() => {
    window.__FORGEBOX_QA__?.bridge.autoRestart();
  });
  await page.waitForSelector('[data-testid="connection-banner"]', { state: "detached" });
  const after = await publish(10);
  const expected = [...before, ...during, ...after];
  await page.waitForFunction(
    (n) => (window.__FORGEBOX_RENDERS__?.ledger().length ?? 0) >= n,
    expected.length,
  );
  await page.waitForTimeout(300);

  const ledger = await page.evaluate(() => window.__FORGEBOX_RENDERS__?.ledger() ?? []);
  const ids = ledger.map((e) => e.id);
  const duplicates = ids.length - new Set(ids).size;
  const seqs = new Set(ledger.map((e) => e.seq));
  const missing = expected.filter((seq) => !seqs.has(seq)).length;
  report.record("duplicateActivityEntries", duplicates, "entries", "max", 0);
  report.record("missingActivityEntries", missing, "entries", "max", 0);
  report.note("eventsExpected", expected.length);
  report.note("ledgerSize", ledger.length);
  report.finish();
  expect(ledger.length).toBeGreaterThanOrEqual(expected.length);
});
