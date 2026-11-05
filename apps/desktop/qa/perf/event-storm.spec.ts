// Budget "events applied in batch per frame, no global re-render" (docs/07 section 10): 500
// events/s on one service instance cause at most one commit of its row per animation frame and
// no render of any other row. Uses the qa-only render counter (src/qa/render-counter.ts).
import { expect, maxOf, openApp, PerfReport, perfUrl, readProbe, startProbe, test } from "./lib";

const EVENTS_PER_SECOND = 500;
const DURATION_MS = 2000;
const TICK_MS = 10;
const MAX_COMMITS_PER_FRAME = 1;
const MAX_LONG_TASK_MS = 50;

test("500 events/s: one commit per frame for the entity, none elsewhere", async ({ page }) => {
  const report = new PerfReport("event-storm");
  await openApp(page, perfUrl("/services", "heavy"));
  await page.waitForSelector("[data-service-row]");
  const rows = await page.locator("[data-service-row]").count();
  expect(rows).toBeGreaterThan(5);
  const touched =
    (await page.locator("[data-service-row]").first().getAttribute("data-service-row")) ?? "";
  expect(touched).not.toBe("");
  // The metrics poll (services.status every 2 s, only while the window is focused) gives every row
  // a new sample by design; this spec is about events, so the window loses focus (still visible,
  // so frames keep batching).
  await page.evaluate(() => {
    document.hasFocus = () => false;
    window.dispatchEvent(new Event("blur"));
  });
  await page.waitForTimeout(100);
  await startProbe(page);
  await page.evaluate(() => {
    window.__FORGEBOX_RENDERS__?.reset();
  });

  const published = await page.evaluate(
    ({ id, perSecond, duration, tick }) =>
      new Promise<number>((resolve) => {
        const qa = window.__FORGEBOX_QA__;
        const inst = qa?.data.instances.find((i) => i.id === id);
        if (qa === undefined || inst === undefined) throw new Error("no qa handle or instance");
        let sent = 0;
        const started = performance.now();
        // Timers are coarse in a headless page: publish what the rate owes at each tick.
        const timer = setInterval(() => {
          const elapsed = performance.now() - started;
          const due = Math.floor((Math.min(elapsed, duration) * perSecond) / 1000);
          while (sent < due) {
            const running = sent % 2 === 0;
            qa.bridge.publish(running ? "service.running" : "service.starting", {
              v: 1,
              instance_id: inst.id,
              name: `${inst.tool}/${inst.name}`,
              tool: inst.tool,
              state: running ? "running" : "starting",
              pid: running ? 2000 : null,
              strategy: null,
              exit_code: null,
              error: null,
            });
            sent += 1;
          }
          if (elapsed >= duration) {
            clearInterval(timer);
            resolve(sent);
          }
        }, tick);
      }),
    { id: touched, perSecond: EVENTS_PER_SECOND, duration: DURATION_MS, tick: TICK_MS },
  );
  await page.waitForTimeout(300);
  const probe = await readProbe(page);
  const stats = await page.evaluate(() => ({
    renders: window.__FORGEBOX_RENDERS__?.renders() ?? {},
    frames: window.__FORGEBOX_RENDERS__?.frames() ?? 0,
    ledger: window.__FORGEBOX_RENDERS__?.ledger().length ?? 0,
  }));

  const key = `instance:${touched}`;
  const own = stats.renders[key];
  const otherRenders = Object.entries(stats.renders)
    .filter(([name]) => name !== key)
    .reduce((sum, [, stat]) => sum + stat.total, 0);
  expect(own, "the touched row rendered").toBeDefined();
  expect(published).toBeGreaterThanOrEqual(EVENTS_PER_SECOND * (DURATION_MS / 1000) * 0.9);
  expect(stats.ledger, "events reached the stores").toBeGreaterThanOrEqual(published);

  report.record(
    "touchedCommitsPerFrameMax",
    own?.maxPerFrame ?? 0,
    "commits",
    "max",
    MAX_COMMITS_PER_FRAME,
  );
  report.record("otherRowRenders", otherRenders, "renders", "max", 0);
  report.record("longTaskMaxMs", maxOf(probe.longTasks), "ms", "max", MAX_LONG_TASK_MS);
  report.note("eventsPublished", published);
  report.note("touchedRenders", own?.total ?? 0);
  report.note("rowsMounted", rows);
  report.note("framesElapsed", stats.frames);
  report.finish();
});
