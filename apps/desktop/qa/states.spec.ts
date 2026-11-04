// State stills of the full Design QA review (M2-E4-T7, docs/design/qa/M2-E4-report.md): every
// screen of the matrix in each fakeBridge state the shot matrix does not cover (empty, loading,
// error, not_implemented, remote, daemon down, heavy), light and dark, DPR 1, 1280x800, pt-BR.
// The `loading` still is taken while the `slow` scenario (1.5 s per call) is still answering.
// Output: qa/out/states/<run>/<screen>/<state>-<theme>-1280x800.png plus one contact sheet per
// screen (the qa:shots teardown). Run: `pnpm -C apps/desktop exec playwright test -c
// qa/playwright.states.config.ts [--grep <screen>]`.
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { SCREENS, type ScreenDef, type Theme } from "./matrix";

/** Same fixed clock as the shot matrix. */
const FIXED_TIME = new Date("2026-03-14T12:00:00.000Z");
const FREEZE_CSS =
  "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}";
const RUN_DIR = process.env.QA_SHOTS_DIR ?? path.resolve("qa/out/states/manual");

/** A review state: the fakeBridge scenario behind it and when the still is taken. */
export const STATES = [
  { id: "empty", scenario: "empty", settle: true },
  { id: "loading", scenario: "slow", settle: false },
  { id: "errors", scenario: "errors", settle: true },
  { id: "not-implemented", scenario: "not-implemented", settle: true },
  { id: "remote", scenario: "remote", settle: true },
  { id: "daemon-down", scenario: "daemon-down", settle: true },
  { id: "heavy", scenario: "heavy", settle: true },
] as const;

const THEMES: readonly Theme[] = ["light", "dark"];

async function shoot(page: Page, screen: ScreenDef, state: (typeof STATES)[number]) {
  await page.clock.setFixedTime(FIXED_TIME);
  const extra = screen.query === undefined ? "" : `&${screen.query}`;
  await page.goto(`${screen.path}?scenario=${state.scenario}&vibrancy=on&motion=full${extra}`);
  await page.addStyleTag({ content: FREEZE_CSS });
  await page.locator("#root > *").first().waitFor();
  if (state.settle) {
    await page.waitForLoadState("networkidle");
    await page.waitForTimeout(600);
  } else {
    // Inside the first 1.5 s answer of the slow scenario: skeletons and loading texts.
    await page.waitForTimeout(450);
  }
  return page.screenshot({ animations: "disabled", caret: "hide", scale: "device" });
}

for (const screen of SCREENS) {
  for (const state of STATES) {
    for (const theme of THEMES) {
      const name = `${state.id}-${theme}-1280x800`;
      test(`${screen.id} ${name}`, async ({ browser }) => {
        const context = await browser.newContext({
          viewport: { width: 1280, height: 800 },
          deviceScaleFactor: 1,
          locale: "pt-BR",
          timezoneId: "UTC",
          colorScheme: theme,
        });
        try {
          const page = await context.newPage();
          const errors: string[] = [];
          page.on("pageerror", (error) => errors.push(error.message));
          const png = await shoot(page, screen, state);
          const file = `${screen.id}/${name}.png`;
          mkdirSync(path.join(RUN_DIR, screen.id), { recursive: true });
          writeFileSync(path.join(RUN_DIR, file), png);
          const cells = path.join(RUN_DIR, ".cells");
          mkdirSync(cells, { recursive: true });
          writeFileSync(
            path.join(cells, `${screen.id}__${name}.json`),
            JSON.stringify({
              screen: screen.id,
              file,
              name,
              informative: false,
              width: 1280,
              height: 800,
              sha256: createHash("sha256").update(png).digest("hex"),
            }),
          );
          expect(errors, "page errors").toEqual([]);
        } finally {
          await context.close();
        }
      });
    }
  }
}
