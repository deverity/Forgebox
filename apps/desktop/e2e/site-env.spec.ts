// The site "Ambiente" tab on the qa build (M2-E3-T20): secrets masked, explicit reveal, copy
// after reveal and the CLI hint. Shots land in qa/out/site-env.
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/site-env";
const SITE = "01QAS1TE000000000000000001";

function url(): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US" });
  return `/sites/${SITE}?${params.toString()}`;
}

test("secrets are masked until revealed, then copyable", async ({ page }) => {
  await page.goto(url());
  await page.getByRole("tab", { name: "Environment" }).click();
  const tab = page.getByTestId("site-env-tab");
  await expect(tab.getByText("APP_ENV")).toBeVisible();
  await expect(tab.getByTestId("env-masked")).toHaveCount(2);
  await expect(tab.getByText("s3cr3t-qa-value")).toHaveCount(0);
  await expect(tab.getByRole("button", { name: "Copy value of DB_PASSWORD" })).toHaveCount(0);
  await page.screenshot({ path: `${SHOT_DIR}/masked.png` });

  await tab.getByRole("button", { name: "Reveal DB_PASSWORD" }).click();
  await expect(tab.getByText("s3cr3t-qa-value")).toBeVisible();
  await expect(tab.getByTestId("env-masked")).toHaveCount(1);
  await expect(tab.getByRole("button", { name: "Copy value of DB_PASSWORD" })).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/revealed.png` });

  await tab.getByRole("button", { name: "Hide DB_PASSWORD" }).click();
  await expect(tab.getByTestId("env-masked")).toHaveCount(2);
  await expect(tab.getByText("forge site env loja.test")).toBeVisible();
});
