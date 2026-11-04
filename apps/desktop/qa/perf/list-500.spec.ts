// Budget "60 fps in 500-item lists" (docs/07 section 10): scrolling the 500-site list down and
// back up keeps the median frame rate at 55 fps or more with no long task above 50 ms.
import { maxOf, medianOf, openApp, PerfReport, perfUrl, readProbe, startProbe, test } from "./lib";

const MIN_FPS = 55;
const MAX_LONG_TASK_MS = 50;
const SCROLL_PX_PER_S = 3000;

// On an RDP session the compositor's vsync is 30 Hz (measured: idle rAF = 32/s), which would
// measure the display and not the list. Uncapping rAF measures the work per frame instead
// (frame time must fit a 60 Hz budget: median >= 55 fps).
test.use({ launchOptions: { args: ["--disable-frame-rate-limit", "--disable-gpu-vsync"] } });

test("500-site list scrolls at 55 fps or more", async ({ page }) => {
  const report = new PerfReport("list-500");
  await openApp(page, perfUrl("/sites", "heavy"));
  await page.locator('[role="list"][tabindex="0"]').first().waitFor();
  await page.waitForFunction(() => document.querySelectorAll('[role="listitem"]').length > 5);
  await startProbe(page);

  // Baseline: the frame rate of the same page at rest, to tell a slow browser from a slow list.
  const idle = await page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        let frames = 0;
        const started = performance.now();
        const step = () => {
          frames += 1;
          if (performance.now() - started >= 1000) resolve(frames);
          else requestAnimationFrame(step);
        };
        requestAnimationFrame(step);
      }),
  );

  const deltas = await page.evaluate(
    (speed) =>
      new Promise<number[]>((resolve) => {
        const el = document.querySelector<HTMLElement>('[role="list"][tabindex="0"]');
        if (el === null) throw new Error("no list");
        const max = el.scrollHeight - el.clientHeight;
        const frames: number[] = [];
        let last = 0;
        let position = 0;
        let direction = 1;
        let passes = 0;
        const step = (now: number) => {
          if (last !== 0) {
            frames.push(now - last);
            position += (direction * speed * (now - last)) / 1000;
          }
          last = now;
          if (position >= max) {
            position = max;
            direction = -1;
            passes += 1;
          } else if (position <= 0 && direction === -1) {
            position = 0;
            direction = 1;
            passes += 1;
          }
          el.scrollTop = position;
          if (passes >= 2 || frames.length > 1500) resolve(frames);
          else requestAnimationFrame(step);
        };
        requestAnimationFrame(step);
      }),
    SCROLL_PX_PER_S,
  );
  const probe = await readProbe(page);
  report.record("medianFps", 1000 / medianOf(deltas), "fps", "min", MIN_FPS);
  report.record("longTaskMaxMs", maxOf(probe.longTasks), "ms", "max", MAX_LONG_TASK_MS);
  report.note("idleFps", idle);
  report.note("p95FrameMs", [...deltas].sort((a, b) => a - b)[Math.floor(deltas.length * 0.95)]);
  report.note("frames", deltas.length);
  report.note("worstFrameMs", maxOf(deltas));
  report.finish();
});
