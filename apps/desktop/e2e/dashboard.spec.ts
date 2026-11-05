// The Dashboard on the qa build (M2-E2-T14): healthy (no Atenção), every Atenção source at once,
// the empty stack, the strip height from skeleton to data and a live event on top of Atividade
// in both locales. Shots land in qa/out/dashboard (gitignored).
import type { QaHandle } from "../src/qa/bootstrap";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/dashboard";

function url(scenario: string, extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({
    scenario,
    locale: "en-US",
    vibrancy: "off",
    ...extra,
  });
  return `/dashboard?${params.toString()}`;
}

const INSTALLED = {
  v: 1,
  job_id: null,
  tool: "php",
  version: "8.3.37",
  version_id: null,
  progress: null,
  error: null,
};

test("healthy: tiles, no Atenção, Projetos ranking", async ({ page }) => {
  await page.goto(url("dashboard"));
  await expect(page.locator("[data-tile]")).toHaveCount(4);
  await expect(page.getByTestId("dashboard-projects").locator("[data-project-row]")).toHaveCount(3);
  await expect(page.getByTestId("machine-cpu")).toBeVisible();
  await expect(page.getByTestId("dashboard-activity").locator("li")).toHaveCount(4);
  await expect(page.getByTestId("dashboard-recent").locator("[data-recent-site]")).toHaveCount(3);
  await expect(page.getByTestId("dashboard-attention")).toHaveCount(0);
  await page.screenshot({ path: `${SHOT_DIR}/healthy.png` });
});

test("attention: each of the 7 sources shows one item with its command", async ({ page }) => {
  await page.goto(url("dashboard-attention"));
  const items = page.locator("[data-attention-item]");
  await expect(items).toHaveCount(7);
  const commands = await items.evaluateAll((nodes) =>
    nodes.map((node) => node.querySelector("[data-forge-cli]")?.getAttribute("data-forge-cli")),
  );
  expect(commands.sort()).toEqual(
    [
      "forge dns status",
      "forge doctor",
      "forge restart redis/default",
      "forge service ports",
      "forge tls ca rotate",
      "forge tls renew --all",
      "forge tool update --check",
    ].sort(),
  );
  await page.screenshot({ path: `${SHOT_DIR}/attention.png` });
});

test("empty stack: explains and points to the catalog", async ({ page }) => {
  await page.goto(url("empty"));
  await expect(page.getByText("No services yet")).toBeVisible();
  await expect(page.getByText("No sites yet")).toBeVisible();
  await expect(page.getByTestId("dashboard-attention")).toHaveCount(0);
  await page.screenshot({ path: `${SHOT_DIR}/empty.png` });
});

test("the tile strip keeps its height from skeleton to data", async ({ page }) => {
  await page.goto(url("dashboard", { latency: "700" }));
  const strip = page.getByTestId("dashboard-tiles").locator("[data-async-view]");
  await expect(strip).toHaveAttribute("data-async-view", "loading");
  const skeleton = await strip.boundingBox();
  await expect(strip).toHaveAttribute("data-async-view", "ready", { timeout: 10_000 });
  const ready = await strip.boundingBox();
  expect(skeleton?.height).toBeGreaterThan(100);
  expect(ready?.height).toBe(skeleton?.height);
});

for (const [locale, sentence, relative] of [
  ["en-US", "PHP 8.3.37 installed", "now"],
  ["pt-BR", "PHP 8.3.37 instalado", "agora"],
] as const) {
  test(`a live tool.install.completed tops Atividade (${locale})`, async ({ page }) => {
    await page.goto(url("dashboard", { locale }));
    const feed = page.getByTestId("dashboard-activity");
    await expect(feed.locator("li")).toHaveCount(4);
    await page.evaluate((payload) => {
      const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
      qa.bridge.publish("tool.install.completed", payload);
      qa.bridge.flush();
    }, INSTALLED);
    const first = feed.locator("li").first();
    await expect(first).toContainText(sentence);
    await expect(first).toContainText(relative);
    await expect(feed.locator("li")).toHaveCount(5);
  });
}
