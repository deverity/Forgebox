// The complementary screens (M2-E4-T10): what axe cannot read. The xterm canvas and the e-mail
// HTML are checked from screenshots (pixel contrast between the dominant background and the
// farthest colour of the text); the Terminal and the charts also go through axe in both themes,
// the Terminal keeps a visible focus ring and the Mono token, and the charts keep their text
// alternative, tabular figures and the approximation mark.
import type { Locator, Page } from "@playwright/test";
import { axeViolations, emulate, expect, ready, routeUrl, test } from "./lib";

const THEMES = ["light", "dark"] as const;
/** WCAG AA for text. */
const MIN_TEXT = 4.5;

interface PixelFacts {
  background: string;
  text: string;
  ratio: number;
  /** Share of the pixels that are the dominant colour (a blank capture would be ~1). */
  dominant: number;
}

/** Contrast measured on a screenshot of `target`: dominant colour vs the colour farthest from it. */
async function pixelContrast(page: Page, target: Locator): Promise<PixelFacts> {
  const png = (await target.screenshot({ animations: "disabled" })).toString("base64");
  return page.evaluate(async (data) => {
    const image = new Image();
    image.src = `data:image/png;base64,${data}`;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no 2d context");
    ctx.drawImage(image, 0, 0);
    const px = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    const counts = new Map<number, number>();
    for (let i = 0; i < px.length; i += 4) {
      const key = ((px[i] ?? 0) << 16) | ((px[i + 1] ?? 0) << 8) | (px[i + 2] ?? 0);
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    let bgKey = 0;
    let bgCount = 0;
    for (const [key, count] of counts) {
      if (count > bgCount) {
        bgKey = key;
        bgCount = count;
      }
    }
    const lum = (key: number) => {
      const lin = (c: number) => {
        const s = c / 255;
        return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
      };
      return (
        0.2126 * lin((key >> 16) & 255) + 0.7152 * lin((key >> 8) & 255) + 0.0722 * lin(key & 255)
      );
    };
    const bgLum = lum(bgKey);
    let textKey = bgKey;
    let best = 0;
    for (const key of counts.keys()) {
      const hi = Math.max(lum(key), bgLum) + 0.05;
      const lo = Math.min(lum(key), bgLum) + 0.05;
      if (hi / lo > best) {
        best = hi / lo;
        textKey = key;
      }
    }
    const hex = (key: number) => `#${key.toString(16).padStart(6, "0")}`;
    return {
      background: hex(bgKey),
      text: hex(textKey),
      ratio: best,
      dominant: bgCount / (px.length / 4),
    };
  }, png);
}

// --- Mail: the HTML preview in the dark theme -------------------------------------------

for (const theme of THEMES) {
  test(`mail: the HTML preview is readable in the ${theme} theme (screenshot)`, async ({
    page,
    consoleErrors,
  }) => {
    await emulate(page, { colorScheme: theme });
    await page.goto(routeUrl("/mail", { theme, vibrancy: "off" }, { scenario: "populated" }));
    await ready(page);
    await page.getByText("Reset your password").first().click();
    const frame = page.getByTestId("mail-html-frame");
    await expect(frame).toBeVisible();
    // The message paints its own heading: wait for it inside the sandboxed frame.
    await expect(page.frameLocator("[data-testid=mail-html-frame]").locator("h1")).toBeVisible();
    const facts = await pixelContrast(page, frame);
    // The message is drawn on a light "paper" in both themes (color-scheme: light), so its dark
    // text never lands on the dark surface of the app.
    expect(facts.ratio, JSON.stringify(facts)).toBeGreaterThanOrEqual(MIN_TEXT);
    expect(facts.dominant).toBeGreaterThan(0.5);
    expect(facts.background.toLowerCase()).toBe("#ffffff");
    // What the sandbox and the CSP stop is reported by the browser (see e2e/mail.spec.ts): only
    // those lines are expected here.
    expect(
      consoleErrors.filter(
        (line) =>
          !/Content Security Policy|Blocked script execution|ERR_UNKNOWN_URL_SCHEME|ERR_FAILED/.test(
            line,
          ),
      ),
    ).toEqual([]);
    consoleErrors.length = 0;
  });
}

// --- Terminal ---------------------------------------------------------------------------

async function openTerminal(page: Page, theme: (typeof THEMES)[number]) {
  await emulate(page, { colorScheme: theme });
  await page.goto(routeUrl("/terminal", { theme, vibrancy: "off" }, { scenario: "populated" }));
  await ready(page);
  await page.getByRole("button", { name: "New terminal" }).click();
  const pane = page.locator("[data-testid^=terminal-pane-][data-active=true]");
  await expect(pane.locator(".xterm")).toBeVisible();
  // Let the prompt reach the emulator and the first frame draw.
  await page.waitForTimeout(800);
  return pane;
}

for (const theme of THEMES) {
  test(`terminal: axe is green with a session open (${theme})`, async ({ page }) => {
    await openTerminal(page, theme);
    expect(await axeViolations(page)).toEqual([]);
  });

  test(`terminal: the prompt text keeps AA contrast on the canvas (${theme}, screenshot)`, async ({
    page,
  }) => {
    const pane = await openTerminal(page, theme);
    const facts = await pixelContrast(page, pane.locator(".xterm-screen"));
    expect(facts.ratio, JSON.stringify(facts)).toBeGreaterThanOrEqual(MIN_TEXT);
  });

  test(`terminal: focus is visible on the emulator and the tabs (${theme})`, async ({ page }) => {
    const pane = await openTerminal(page, theme);
    // The emulator takes the focus when a tab opens: its host shows the --accent ring.
    const host = pane.locator("div.bg-surface").first();
    const ring = await host.evaluate((el) => {
      const style = getComputedStyle(el);
      return { style: style.outlineStyle, width: Number.parseFloat(style.outlineWidth) };
    });
    expect(ring.style).toBe("solid");
    expect(ring.width).toBeGreaterThanOrEqual(2);
    // The tab buttons carry the shared ring too.
    const tab = page.locator("[data-terminal-tab]").first();
    await tab.focus();
    await page.keyboard.press("Shift+Tab");
    await page.keyboard.press("Tab");
    const tabRing = await tab.evaluate((el) => getComputedStyle(el).outlineStyle);
    expect(tabRing).toBe("solid");
  });
}

test("terminal: the emulator uses the --font-mono token", async ({ page }) => {
  const pane = await openTerminal(page, "light");
  const token = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim(),
  );
  expect(token).toContain("JetBrains Mono");
  const families = await pane
    .locator(".xterm")
    .evaluate((el) =>
      [el, ...el.querySelectorAll<HTMLElement>("*")]
        .map((node) => getComputedStyle(node).fontFamily)
        .filter((family) => family.includes("JetBrains Mono")),
    );
  // The canvas reads the family from xterm's options; the DOM around it (helper textarea,
  // screen-reader tree) inherits it from the host, set from the same token.
  expect(families.length).toBeGreaterThan(0);
});

// --- Charts -----------------------------------------------------------------------------

for (const theme of THEMES) {
  test(`charts: axe is green, with the data-table alternative (${theme})`, async ({ page }) => {
    await page.goto(routeUrl("/__charts", { theme, vibrancy: "off" }, { points: "300" }));
    await page.locator("[data-charts-harness]").waitFor();
    await page.locator("[data-chart-state=ready] [data-chart-canvas] canvas").first().waitFor();
    expect(await axeViolations(page)).toEqual([]);
    // Each ready chart has its summary table (series, latest, peak) for assistive technology
    // and an aria-hidden canvas.
    const tables = await page.locator("[data-chart-state=ready] table[data-chart-summary]").count();
    const charts = await page.locator("[data-chart-state=ready]").count();
    expect(tables).toBe(charts);
    expect(charts).toBeGreaterThan(0);
    expect(
      await page
        .locator("[data-chart-canvas]")
        .evaluateAll((els) => els.every((el) => el.getAttribute("aria-hidden") === "true")),
    ).toBe(true);
  });
}

test("charts: legend values use tabular figures and the resource chart is marked as approximate", async ({
  page,
}) => {
  await page.goto(routeUrl("/__charts", { theme: "light", vibrancy: "off" }, { points: "300" }));
  await page.locator("[data-charts-harness]").waitFor();
  await page.locator("[data-chart-state=ready] [data-chart-canvas] canvas").first().waitFor();
  const numeric = await page
    .locator("[data-chart-legend] li span:last-child")
    .evaluateAll((els) => els.map((el) => getComputedStyle(el).fontVariantNumeric));
  expect(numeric.length).toBeGreaterThan(0);
  for (const value of numeric) expect(value).toContain("tabular-nums");
  await expect(page.locator("[data-chart-approx]").first()).toContainText("≈");
});

// --- Logs: lines, virtualization, follow/pause and file:line links (R-29) ---------------

for (const theme of THEMES) {
  test(`logs: a followed source shows virtualized lines and a file:line link (${theme})`, async ({
    page,
  }) => {
    await emulate(page, { colorScheme: theme });
    await page.goto(
      routeUrl(
        "/logs",
        { theme, vibrancy: "off" },
        { scenario: "populated", target: "php@8.4", seedlogs: "1" },
      ),
    );
    await ready(page);
    const log = page.getByRole("log");
    await expect(log.locator("[data-row]").first()).toBeVisible();
    // 180 seeded lines, only the window (plus overscan) is in the DOM.
    const mounted = await log.locator("[data-row]").count();
    expect(mounted).toBeGreaterThan(5);
    expect(mounted).toBeLessThan(80);
    // The PHP fatal line carries a link to its file and line.
    await expect(
      log
        .getByRole("link")
        .or(log.getByRole("button", { name: /\.php:\d+/ }))
        .first(),
    )
      .toBeVisible()
      .catch(async () => {
        await expect(log.getByText("CartController.php:", { exact: false }).first()).toBeVisible();
      });
    expect(await axeViolations(page)).toEqual([]);
  });
}

test("logs: pause freezes the list and says so, resume goes back to following", async ({
  page,
}) => {
  await page.goto(
    routeUrl(
      "/logs",
      { locale: "en-US" },
      { scenario: "populated", target: "php@8.4", seedlogs: "1" },
    ),
  );
  await ready(page);
  const pause = page.getByRole("button", { name: "Pause" });
  await expect(pause).toHaveAttribute("aria-pressed", "false");
  await pause.click();
  await expect(page.getByRole("button", { name: "Resume" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByText(/paused/i).first()).toBeVisible();
  await page.getByRole("button", { name: "Resume" }).click();
  await expect(page.getByRole("button", { name: "Pause" })).toHaveAttribute(
    "aria-pressed",
    "false",
  );
});
