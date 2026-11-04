// /mail in a real browser against the fake bridge (M2-E3-T13): the sandbox of the HTML preview
// (no scripts, a CSP, no request until the per-message toggle, no Tauri bridge inside the
// frame) and the live badge without polling.
import type { Page } from "@playwright/test";
import type { QaHandle } from "../src/qa/bootstrap";
import { expect, test } from "./fixtures";

const URL = "/mail?scenario=populated&locale=en-US";
const REMOTE_HOST = "127.0.0.1:9";

async function openHtmlMessage(page: Page) {
  await page.goto(URL);
  await page.getByText("Reset your password").first().click();
  const frame = page.getByTestId("mail-html-frame");
  await expect(frame).toBeVisible();
  return frame;
}

test("the HTML preview is sandboxed: no scripts, CSP, nothing remote until the toggle", async ({
  page,
  consoleErrors,
}) => {
  const external: string[] = [];
  await page.route(`http://${REMOTE_HOST}/**`, async (route) => {
    external.push(route.request().url());
    await route.abort();
  });
  const frame = await openHtmlMessage(page);

  const sandbox = (await frame.getAttribute("sandbox")) ?? "";
  expect(sandbox.split(/\s+/)).toEqual(["allow-popups", "allow-popups-to-escape-sandbox"]);
  expect(sandbox).not.toContain("allow-scripts");
  expect(sandbox).not.toContain("allow-same-origin");
  const srcdoc = (await frame.getAttribute("srcdoc")) ?? "";
  expect(srcdoc).toContain(
    `content="default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'"`,
  );

  // Inside the frame: the message script never ran and there is no Tauri bridge.
  const handle = await frame.elementHandle();
  const inner = await handle.contentFrame();
  expect(inner).toBeTruthy();
  await expect(page.getByTestId("mail-remote-images")).toContainText("1 remote image blocked");
  await page.waitForTimeout(500);
  expect(external, "no request leaves the frame while images are blocked").toEqual([]);
  const probe = await inner?.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    return {
      tauri: typeof w.__TAURI__,
      internals: typeof w.__TAURI_INTERNALS__,
      ran: w.__ranScript === true,
      clicked: w.__clicked === 1,
      title: document.title,
    };
  });
  expect(probe).toEqual({
    tauri: "undefined",
    internals: "undefined",
    ran: false,
    clicked: false,
    title: "",
  });

  // The toggle widens img-src for this message only; now the image is requested.
  await page.getByRole("button", { name: "Show images" }).click();
  await expect.poll(() => external.length, { timeout: 10_000 }).toBeGreaterThan(0);

  // The browser reports what the sandbox and the CSP stopped: that is the proof, so these
  // console lines are expected here and nothing else may appear.
  const expected =
    /Content Security Policy|Blocked script execution|ERR_UNKNOWN_URL_SCHEME|ERR_FAILED/;
  expect(consoleErrors.filter((line) => !expected.test(line))).toEqual([]);
  expect(consoleErrors.some((line) => /Content Security Policy.*img-src/.test(line))).toBe(true);
  expect(consoleErrors.some((line) => line.includes("Blocked script execution"))).toBe(true);
  consoleErrors.length = 0;
});

test("mail.changed moves the badge with no mail.list polling", async ({ page }) => {
  await page.goto(URL);
  await expect(page.getByTestId("mail-list")).toBeVisible();
  const badge = page.getByTestId("badge-mail");
  await expect(badge).toHaveText("2");

  const count = (method: string) =>
    page.evaluate((m) => {
      const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
      return qa.bridge.calls.filter((c) => c.method === m).length;
    }, method);

  // Let the screen settle, then watch a quiet period: nothing asks for the list.
  await page.waitForTimeout(500);
  const before = await count("mail.list");
  await page.waitForTimeout(3000);
  expect(await count("mail.list")).toBe(before);

  // Another client reads a message: the daemon (here the fake mailbox) publishes mail.changed.
  await page.evaluate(async () => {
    const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
    const list = (await qa.bridge.call({
      context: "local",
      method: "mail.list",
      params: { unread: true },
      request_id: "01HZ3NDEKTSV4RRFFQ69G5FAV1",
    })) as { messages: { id: string }[] };
    await qa.bridge.call({
      context: "local",
      method: "mail.mark_read",
      params: { ids: [list.messages[0]?.id ?? ""], read: true },
      request_id: "01HZ3NDEKTSV4RRFFQ69G5FAV0",
    });
    qa.bridge.flush();
  });
  await expect(badge).toHaveText("1");
  // The list refetched once for the event, not on a timer.
  await page.waitForTimeout(500);
  const after = await count("mail.list");
  await page.waitForTimeout(2000);
  expect(await count("mail.list")).toBe(after);
});
