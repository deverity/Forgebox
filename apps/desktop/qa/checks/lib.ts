// The qa:check rules (M2-E4-T3, docs/07 section 11). Every function returns the list of
// violations found on the current page: the specs assert [] on the real app and a non-empty
// list on the matching negative case of `/qa/selftest`, so a rule that stops detecting fails
// its own self-test.
import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";
import { FEATURES } from "../../src/app/features";
import { SELFTEST_PATH, type SelftestCase } from "../../src/qa/selftest/cases";
import type { QaUrl } from "../../e2e/fixtures";

export { expect, test, emulate, qaUrl, SCENARIOS } from "../../e2e/fixtures";

/** WCAG 2.1 AA colours of docs/07 section 2.3. */
export const SYSTEM_ACCENTS = ["#0078D4", "#FFB900", "#E81123", "#00CC6A"] as const;
export const MIN_TEXT_CONTRAST = 4.5;
export const MAX_CLS = 0.01;
export const REDUCED_MOTION_MAX_MS = 100;

/** Concrete id for the `$id` routes (placeholders ignore it; screens read it from the fixtures). */
const SAMPLE_ID = "demo";

/** Every route of the app: the sidebar/feature table, the dynamic routes, the gallery. */
export const ROUTES: readonly string[] = [
  ...FEATURES.map((f) => f.path),
  `/sites/${SAMPLE_ID}`,
  `/catalog/${SAMPLE_ID}`,
  "/__gallery",
];

export function routeUrl(
  path: string,
  options: QaUrl = {},
  extra: Record<string, string> = {},
): string {
  const params = new URLSearchParams(extra);
  for (const [key, value] of Object.entries(options)) params.set(key, String(value));
  const query = params.toString();
  return query === "" ? path : `${path}?${query}`;
}

export function selftestUrl(which: SelftestCase, options: QaUrl = {}): string {
  return routeUrl(SELFTEST_PATH, options, { case: which });
}

/**
 * The window material (Mica) is drawn by the OS, so in a browser the transparent regions of a
 * `vibrancy=on` page would sit on the white default backdrop and axe would measure dark text
 * colours on white. Paints the opaque fallback token behind the page instead (what the OS
 * material is a lighter/darker version of).
 */
export async function withMaterialBackdrop(page: Page): Promise<void> {
  await page.addStyleTag({ content: "html{background:var(--bg-sidebar-solid)}" });
}

/** Colours and outlines are measured at rest, not halfway through a CSS transition. */
async function freezeTransitions(page: Page): Promise<void> {
  await page.addStyleTag({
    content: "*,*::before,*::after{transition:none!important;animation:none!important}",
  });
}

/** Waits for the screen: the first heading of the main region. */
export async function ready(page: Page): Promise<void> {
  await page.locator("main h1, main [role=heading]").first().waitFor({ timeout: 15_000 });
  // Lazy screens and fonts settle; nothing here depends on the daemon.
  await page.waitForLoadState("networkidle");
}

// ---------------------------------------------------------------------------------------------
// Contrast

/** The page-side helpers are installed once per page by `withColorTools`. */
interface ColorTools {
  resolve(css: string): [number, number, number, number];
}
declare global {
  interface Window {
    __qaColor?: ColorTools;
  }
}

async function withColorTools(page: Page): Promise<void> {
  await page.evaluate(() => {
    if (window.__qaColor) return;
    const ctx = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
    if (!ctx) throw new Error("no 2d canvas");
    window.__qaColor = {
      resolve(css) {
        ctx.clearRect(0, 0, 1, 1);
        ctx.fillStyle = "#000";
        ctx.fillStyle = css;
        ctx.fillRect(0, 0, 1, 1);
        const [r = 0, g = 0, b = 0, a = 0] = ctx.getImageData(0, 0, 1, 1).data;
        return [r, g, b, a / 255];
      },
    };
  });
}

type Rgb = readonly [number, number, number];

function channel(value: number): number {
  const c = value / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

export function luminance([r, g, b]: Rgb): number {
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

export function contrastRatio(a: Rgb, b: Rgb): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * The fg tokens over the opaque sidebar fallback (`--bg-sidebar-solid`, what a window without
 * material paints) must reach 4.5:1. `scope` is the element whose custom properties are read.
 */
export async function sidebarContrastViolations(page: Page, scope = "html"): Promise<string[]> {
  await withColorTools(page);
  const read = await page.evaluate((selector) => {
    const el = document.querySelector(selector);
    if (!el) return null;
    const style = getComputedStyle(el);
    const tokens = ["--fg-primary", "--fg-secondary", "--fg-tertiary"] as const;
    const tools = window.__qaColor;
    if (!tools) throw new Error("color tools missing");
    const color = (css: string) => tools.resolve(css);
    return {
      bg: color(style.getPropertyValue("--bg-sidebar-solid").trim()),
      fg: tokens.map((token) => [token, color(style.getPropertyValue(token).trim())] as const),
    };
  }, scope);
  if (read === null) return [`scope ${scope} not found`];
  const bg: Rgb = [read.bg[0], read.bg[1], read.bg[2]];
  const out: string[] = [];
  for (const [token, rgba] of read.fg) {
    const ratio = contrastRatio([rgba[0], rgba[1], rgba[2]], bg);
    if (ratio < MIN_TEXT_CONTRAST) {
      out.push(
        `${token} on --bg-sidebar-solid is ${ratio.toFixed(2)}:1 (< ${String(MIN_TEXT_CONTRAST)})`,
      );
    }
  }
  return out;
}

/**
 * Text on `--accent`: for each Windows system accent, applies it like the shell does and measures
 * the rendered colour of every `selector` element over its own background.
 */
export async function accentContrastViolations(
  page: Page,
  selector: string,
  accents: readonly string[] = SYSTEM_ACCENTS,
): Promise<string[]> {
  await withColorTools(page);
  await freezeTransitions(page);
  const out: string[] = [];
  for (const accent of accents) {
    await page.evaluate((hex) => {
      const qa = window.__FORGEBOX_QA__;
      if (!qa) throw new Error("not a qa build");
      qa.setAccent(hex);
    }, accent);
    const samples = await page.evaluate((sel) => {
      const tools = window.__qaColor;
      if (!tools) throw new Error("color tools missing");
      const color = (css: string) => tools.resolve(css);
      return [...document.querySelectorAll<HTMLElement>(sel)].map((el) => {
        const style = getComputedStyle(el);
        return { fg: color(style.color), bg: color(style.backgroundColor) };
      });
    }, selector);
    if (samples.length === 0) out.push(`no element matches ${selector}`);
    for (const { fg, bg } of samples) {
      const ratio = contrastRatio([fg[0], fg[1], fg[2]], [bg[0], bg[1], bg[2]]);
      if (ratio < MIN_TEXT_CONTRAST) {
        out.push(
          `text on accent ${accent} is ${ratio.toFixed(2)}:1 (< ${String(MIN_TEXT_CONTRAST)})`,
        );
      }
    }
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// axe

export async function axeViolations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  return result.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id} (${v.impact ?? ""}): ${v.help} [${String(v.nodes.length)} nodes]`);
}

// ---------------------------------------------------------------------------------------------
// Keyboard

const INTERACTIVE = [
  "a[href]",
  "button",
  "input:not([type=hidden]):not([type=radio])",
  "select",
  "textarea",
  "summary",
  "[tabindex]",
  "[role=button]",
  "[role=link]",
  "[role=switch]",
  "[role=checkbox]",
  "[role=combobox]",
  "[role=slider]",
].join(",");

/** Containers whose items share one tab stop (roving tabindex): only the container is checked. */
const ROVING = "[role=tablist],[role=radiogroup],[role=listbox],[role=menu],[role=toolbar]";

interface TabReport {
  unreachable: string[];
  noRing: string[];
}

/**
 * Presses Tab through the page: every enabled, visible, natively or explicitly focusable control
 * must receive focus, and each must show a visible `--accent` focus ring (outline or box-shadow).
 */
export async function keyboardViolations(page: Page): Promise<string[]> {
  const total = await page.evaluate(
    ({ interactive, roving }) => {
      let n = 0;
      document.querySelectorAll<HTMLElement>(interactive).forEach((el) => {
        const rect = el.getBoundingClientRect();
        const hidden =
          rect.width === 0 ||
          rect.height === 0 ||
          el.closest("[aria-hidden=true],[inert]") !== null ||
          getComputedStyle(el).visibility === "hidden";
        const disabled =
          (el as HTMLButtonElement).disabled || el.matches("[data-disabled],[aria-disabled=true]");
        const skipped =
          el.getAttribute("tabindex") === "-1" ||
          (el.closest(roving) !== null && el.closest(roving) !== el);
        if (hidden || disabled || skipped) return;
        el.dataset.qaTab = String(n);
        n += 1;
      });
      return n;
    },
    { interactive: INTERACTIVE, roving: ROVING },
  );
  await withColorTools(page);
  await freezeTransitions(page);
  await page.evaluate(() => {
    (document.activeElement as HTMLElement | null)?.blur();
  });
  const reached = new Set<string>();
  const noRing: string[] = [];
  for (let i = 0; i < total + 6; i += 1) {
    await page.keyboard.press("Tab");
    const info = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      if (!el || el === document.body) return null;
      const style = getComputedStyle(el);
      const tools = window.__qaColor;
      if (!tools) throw new Error("color tools missing");
      const accent = tools.resolve(
        getComputedStyle(document.documentElement).getPropertyValue("--accent").trim(),
      );
      const outline = tools.resolve(style.outlineColor);
      const outlineVisible =
        style.outlineStyle !== "none" &&
        style.outlineStyle !== "hidden" &&
        parseFloat(style.outlineWidth) >= 1 &&
        outline[0] === accent[0] &&
        outline[1] === accent[1] &&
        outline[2] === accent[2];
      const shadowVisible =
        style.boxShadow !== "none" &&
        style.boxShadow.includes(`rgb(${accent.slice(0, 3).join(", ")})`);
      const label = `${el.tagName.toLowerCase()}${el.getAttribute("role") ? `[${el.getAttribute("role") ?? ""}]` : ""} "${(el.getAttribute("aria-label") ?? el.textContent).trim().slice(0, 30)}"`;
      // A roving group (radiogroup...) counts as reached when focus is inside it.
      return {
        tag: el.closest<HTMLElement>("[data-qa-tab]")?.dataset.qaTab ?? null,
        ring: outlineVisible || shadowVisible,
        label,
      };
    });
    if (info === null) continue;
    if (info.tag !== null) reached.add(info.tag);
    if (!info.ring && !noRing.includes(info.label)) noRing.push(info.label);
  }
  const missing = await page.evaluate(
    (seen) => {
      return [...document.querySelectorAll<HTMLElement>("[data-qa-tab]")]
        .filter((el) => !seen.includes(el.dataset.qaTab ?? ""))
        .map(
          (el) =>
            `${el.tagName.toLowerCase()} "${(el.getAttribute("aria-label") ?? el.textContent).trim().slice(0, 30)}"`,
        );
    },
    [...reached],
  );
  const report: TabReport = { unreachable: missing, noRing };
  return [
    ...report.unreachable.map((label) => `Tab never reaches ${label}`),
    ...report.noRing.map((label) => `no visible --accent focus ring on ${label}`),
  ];
}

/**
 * Every popup trigger (`aria-haspopup`) opens on Enter, closes on Escape and gets the focus
 * back.
 */
export async function escapeViolations(page: Page): Promise<string[]> {
  const count = await page.locator("[aria-haspopup]:not([aria-haspopup=false])").count();
  const out: string[] = [];
  for (let i = 0; i < count; i += 1) {
    const trigger = page.locator("[aria-haspopup]:not([aria-haspopup=false])").nth(i);
    if (!(await trigger.isVisible()) || (await trigger.isDisabled())) continue;
    const name = (await trigger.textContent())?.trim().slice(0, 30) ?? String(i);
    await trigger.focus();
    await page.keyboard.press("Enter");
    const opened = await page
      .waitForFunction(
        (index) =>
          [...document.querySelectorAll("[aria-haspopup]:not([aria-haspopup=false])")][
            index
          ]?.getAttribute("aria-expanded") === "true",
        i,
        { timeout: 1500 },
      )
      .then(
        () => true,
        () => false,
      );
    if (!opened) {
      out.push(`trigger "${name}" does not open with Enter`);
      continue;
    }
    await page.keyboard.press("Escape");
    const closed = await page
      .waitForFunction(
        (index) =>
          [...document.querySelectorAll("[aria-haspopup]:not([aria-haspopup=false])")][
            index
          ]?.getAttribute("aria-expanded") !== "true",
        i,
        { timeout: 1500 },
      )
      .then(
        () => true,
        () => false,
      );
    if (!closed) {
      out.push(`Escape does not close the surface of "${name}"`);
      // Leave nothing open behind us: the next trigger would be unreachable.
      await page.reload();
      await ready(page);
      continue;
    }
    // Radix returns the focus when the exit animation ends, so give it a moment.
    const focusBack = await page
      .waitForFunction(
        (index) =>
          document.activeElement ===
          [...document.querySelectorAll("[aria-haspopup]:not([aria-haspopup=false])")][index],
        i,
        { timeout: 1500 },
      )
      .then(
        () => true,
        () => false,
      );
    if (!focusBack) out.push(`focus does not return to "${name}" after Escape`);
  }
  return out;
}

/** Section 8 shortcuts that must work in the app. `effect` null: not implemented yet. */
export interface ShortcutCheck {
  keys: string;
  label: string;
  /** Selector that must become visible after the keys are pressed. */
  effect: string | null;
  /** Roadmap task that delivers it (shown in the report while `effect` is null). */
  pending?: string;
}

export async function shortcutViolations(
  page: Page,
  shortcuts: readonly ShortcutCheck[],
): Promise<string[]> {
  const out: string[] = [];
  for (const shortcut of shortcuts) {
    if (shortcut.effect === null) continue;
    await page.keyboard.press(shortcut.keys);
    const shown = await page
      .locator(shortcut.effect)
      .first()
      .waitFor({ state: "visible", timeout: 1500 })
      .then(
        () => true,
        () => false,
      );
    if (!shown) out.push(`${shortcut.keys} (${shortcut.label}) has no effect`);
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// Layout shift

/** Cumulative layout shift of loading `url` and letting data arrive (see `latency`). */
export async function clsOf(page: Page, url: string, settleMs = 1500): Promise<number> {
  await page.addInitScript(() => {
    const w = window as unknown as { __cls: number };
    w.__cls = 0;
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const shift = entry as PerformanceEntry & { value: number; hadRecentInput: boolean };
        if (!shift.hadRecentInput) w.__cls += shift.value;
      }
    }).observe({ type: "layout-shift", buffered: true });
  });
  await page.goto(url);
  await ready(page);
  await page.waitForTimeout(settleMs);
  return page.evaluate(() => (window as unknown as { __cls: number }).__cls);
}

// ---------------------------------------------------------------------------------------------
// Motion

/**
 * Records every running animation (CSS animations, transitions and Web Animations, which is
 * what Motion uses for opacity and transform) from now on. Install before the page loads.
 */
export async function recordAnimations(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const seen = new Map<string, string>();
    (window as unknown as { __anims: Map<string, string> }).__anims = seen;
    const meta = new Set(["offset", "easing", "composite", "computedOffset"]);
    const sample = () => {
      for (const animation of document.getAnimations()) {
        const effect = animation.effect as KeyframeEffect | null;
        if (!effect) continue;
        const timing = effect.getComputedTiming();
        const total =
          timing.iterations === Infinity
            ? Infinity
            : (timing.delay ?? 0) +
              Number(timing.duration) * (timing.iterations ?? 1) +
              (timing.endDelay ?? 0);
        const props = new Set<string>();
        for (const frame of effect.getKeyframes()) {
          for (const key of Object.keys(frame)) if (!meta.has(key)) props.add(key);
        }
        const name =
          (animation as Partial<CSSAnimation>).animationName ??
          (animation as Partial<CSSTransition>).transitionProperty ??
          animation.id;
        const target = (effect.target as HTMLElement | null)?.tagName.toLowerCase() ?? "?";
        seen.set(`${name} on ${target}`, JSON.stringify({ total, props: [...props].sort() }));
      }
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  });
}

/** Under reduced motion: every recorded animation is <= 100 ms and animates opacity only. */
export async function motionViolations(page: Page): Promise<string[]> {
  const entries = await page.evaluate(() => {
    const seen = (window as unknown as { __anims?: Map<string, string> }).__anims;
    return seen ? [...seen.entries()] : [];
  });
  const out: string[] = [];
  for (const [name, json] of entries) {
    const { total, props } = JSON.parse(json) as { total: number | null; props: string[] };
    // JSON turns Infinity into null.
    if (total === null || total > REDUCED_MOTION_MAX_MS) {
      out.push(
        `${name} lasts ${total === null ? "forever" : `${String(total)} ms`} (> ${String(REDUCED_MOTION_MAX_MS)})`,
      );
    }
    const other = props.filter((p) => p !== "opacity");
    if (other.length > 0) out.push(`${name} animates ${other.join(", ")} (opacity only)`);
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// DOM contracts

/** Action controls, error states and (remote) local-only actions. */
export async function contractViolations(page: Page, remote = false): Promise<string[]> {
  return page.evaluate((isRemote) => {
    const out: string[] = [];
    const describe = (el: Element) =>
      el.getAttribute("data-action") ??
      el.getAttribute("data-error-code") ??
      el.tagName.toLowerCase();
    document.querySelectorAll("[data-action]").forEach((el) => {
      if ((el.getAttribute("data-forge-cli") ?? "").trim() === "") {
        out.push(`action ${describe(el)} has no data-forge-cli`);
      }
    });
    document.querySelectorAll("[data-error-notice]").forEach((el) => {
      if ((el.getAttribute("data-error-code") ?? "").trim() === "") {
        out.push("error state without data-error-code");
      }
      const cause = el.querySelector("p")?.textContent.trim() ?? "";
      if (cause === "") out.push(`error ${describe(el)} has no cause text`);
      if (el.querySelectorAll("button").length === 0) {
        out.push(`error ${describe(el)} offers no action`);
      }
    });
    if (isRemote) {
      document.querySelectorAll('[data-local-only="true"]').forEach((el) => {
        const reason = (el.getAttribute("data-disabled-reason") ?? "").trim();
        const button = el.querySelector("button");
        if (reason === "") out.push(`local_only action ${describe(el)} has no explanation`);
        if (button !== null && !button.disabled) {
          out.push(`local_only action ${describe(el)} is enabled on a remote context`);
        }
      });
      const text = [
        document.body.innerText,
        ...[...document.querySelectorAll("input,textarea")].map(
          (el) => (el as HTMLInputElement).value,
        ),
      ].join("\n");
      const path = /(^|[^A-Za-z0-9])[A-Za-z]:\\[^\s]*/.exec(text);
      if (path !== null) out.push(`client path on a remote context: ${path[0].trim()}`);
    }
    return out;
  }, remote);
}
