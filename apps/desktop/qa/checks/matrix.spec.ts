// M2-E4-T6: the shell, the gallery, the palette, the banners and the collapsed sidebar under the
// rendering matrix of docs/07 section 12: light/dark x vibrancy off x reduced motion x DPR
// 1/1.25/1.5. axe (WCAG 2.1 AA), the fg token contrast over the opaque sidebar and the Tab/focus
// ring traversal run in every cell; the DPR is the real device scale factor of the context.
import {
  axeViolations,
  expect,
  keyboardViolations,
  ready,
  routeUrl,
  sidebarContrastViolations,
  test,
  type QaUrl,
} from "./lib";

const DPRS = [1, 1.25, 1.5] as const;
const THEMES = ["light", "dark"] as const;
/** Shell screens and the gallery (the screens of the other epics are covered by axe.spec.ts). */
const SCREENS = ["/dashboard", "/services", "/sites", "/settings", "/__gallery"] as const;
const MODES: readonly { name: string; url: QaUrl }[] = [
  { name: "vibrancy-off", url: { vibrancy: "off", motion: "full" } },
  { name: "reduced-motion", url: { vibrancy: "off", motion: "reduced" } },
];

for (const dpr of DPRS) {
  test.describe(`DPR ${String(dpr)}`, () => {
    test.use({ deviceScaleFactor: dpr, viewport: { width: 1280, height: 800 } });

    for (const screen of SCREENS) {
      for (const theme of THEMES) {
        for (const mode of MODES) {
          test(`axe + sidebar contrast: ${screen} ${theme} ${mode.name}`, async ({ page }) => {
            await page.goto(routeUrl(screen, { scenario: "populated", theme, ...mode.url }));
            await ready(page);
            expect(await page.evaluate(() => window.devicePixelRatio)).toBe(dpr);
            expect(await axeViolations(page)).toEqual([]);
            expect(await sidebarContrastViolations(page)).toEqual([]);
          });
        }
      }
    }

    for (const theme of THEMES) {
      test(`tab order and focus ring: shell + gallery ${theme}`, async ({ page }) => {
        for (const screen of ["/dashboard", "/__gallery"]) {
          await page.goto(
            routeUrl(screen, { scenario: "populated", theme, vibrancy: "off", motion: "reduced" }),
          );
          await ready(page);
          expect(await keyboardViolations(page), screen).toEqual([]);
        }
      });

      test(`command palette open: ${theme}`, async ({ page }) => {
        await page.goto(routeUrl("/dashboard", { scenario: "populated", theme, vibrancy: "off" }));
        await ready(page);
        await page.keyboard.press("Control+k");
        await page.locator("[role=dialog] [cmdk-input]").waitFor();
        expect(await axeViolations(page)).toEqual([]);
        expect(await keyboardViolations(page)).toEqual([]);
      });

      test(`connection banner (daemon down): ${theme}`, async ({ page }) => {
        await page.goto(
          routeUrl("/dashboard", { scenario: "daemon-down", theme, vibrancy: "off" }),
        );
        await ready(page);
        expect(await axeViolations(page)).toEqual([]);
      });

      test(`collapsed sidebar: ${theme}`, async ({ page }) => {
        await page.setViewportSize({ width: 1000, height: 700 });
        await page.goto(routeUrl("/dashboard", { scenario: "populated", theme, vibrancy: "off" }));
        await ready(page);
        const width = await page
          .locator("aside, nav")
          .first()
          .evaluate((el) => el.getBoundingClientRect().width);
        expect(width).toBeLessThan(80);
        expect(await axeViolations(page)).toEqual([]);
        expect(await keyboardViolations(page)).toEqual([]);
      });
    }
  });
}

// The work-area cases of docs/07 section 4: the minimum window is min(960x600, work area), so a
// 1366x768 laptop at 125-150 % (taskbar included) leaves 960x540 and 1093x574 of client area.
// The layout degrades to the collapsed sidebar and scrolls inside the content, never the page.
const WORK_AREAS = [
  { width: 960, height: 540 },
  { width: 1093, height: 574 },
] as const;

for (const size of WORK_AREAS) {
  for (const theme of THEMES) {
    for (const screen of ["/dashboard", "/services", "/sites", "/settings", "/__gallery"]) {
      test(`layout at ${String(size.width)}x${String(size.height)}: ${screen} ${theme}`, async ({
        page,
      }) => {
        await page.setViewportSize(size);
        await page.goto(routeUrl(screen, { scenario: "heavy", theme, vibrancy: "off" }));
        await ready(page);
        const layout = await page.evaluate(() => {
          const rect = (id: string) =>
            document.querySelector(`[data-testid=${id}]`)?.getBoundingClientRect() ?? null;
          const doc = document.documentElement;
          return {
            pageOverflowX: doc.scrollWidth - doc.clientWidth,
            pageOverflowY: doc.scrollHeight - doc.clientHeight,
            bodyOverflowY: document.body.scrollHeight - window.innerHeight,
            sidebar: rect("sidebar"),
            titlebar: rect("titlebar"),
            pane: rect("content-pane"),
            sidebarStatus: rect("sidebar-status"),
            statusBar: rect("status-bar"),
            innerHeight: window.innerHeight,
          };
        });
        expect(layout.pageOverflowX).toBeLessThanOrEqual(0);
        expect(layout.pageOverflowY).toBeLessThanOrEqual(0);
        expect(layout.bodyOverflowY).toBeLessThanOrEqual(0);
        // The gallery is a standalone page (no shell): only the shell screens have these parts.
        if (screen !== "/__gallery") {
          expect(layout.sidebar?.width ?? 999).toBeLessThan(80);
          // M2-E1-T23: the sidebar runs the full height from the top edge; the toolbar sits on
          // top of the content pane only (no full-width titlebar), and there is no status bar.
          expect(layout.sidebar?.top).toBe(0);
          expect(Math.round(layout.sidebar?.bottom ?? 0)).toBe(layout.innerHeight);
          expect(layout.titlebar?.top).toBe(0);
          expect(Math.round(layout.titlebar?.left ?? 0)).toBe(
            Math.round(layout.sidebar?.right ?? -1),
          );
          expect(Math.round(layout.titlebar?.right ?? 0)).toBe(
            Math.round(layout.pane?.right ?? -1),
          );
          expect(layout.statusBar).toBeNull();
          // Its functions (daemon, CPU/RAM, DNS, CA) stay visible in the sidebar footer.
          expect(layout.sidebarStatus?.bottom ?? 999).toBeLessThanOrEqual(layout.innerHeight);
        }
        expect(await axeViolations(page)).toEqual([]);
      });
    }
  }
}
