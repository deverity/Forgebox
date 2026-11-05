// Every QA scenario opens without console errors and behaves as scripted (ADR-0036).
import type { QaHandle } from "../src/qa/bootstrap";
import { expect, qaUrl, SCENARIOS, test, type Scenario } from "./fixtures";

interface Probe {
  sentinel: string;
  scenario: string;
  state: { status: string };
  hello: { local: boolean } | null;
  sites: number | null;
  instances: number | null;
  rejection: { code: string; reason: string | null } | null;
}

/** Runs inside the page against the fake bridge installed by src/qa/bootstrap.ts. */
async function probe(page: import("@playwright/test").Page, method: string): Promise<Probe> {
  return page.evaluate(async (m) => {
    const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
    const state = await qa.bridge.state("local");
    let sites: number | null = null;
    let instances: number | null = null;
    let rejection: Probe["rejection"] = null;
    try {
      const request = (name: string) => ({
        context: "local",
        method: name,
        params: {},
        request_id: "01JQA0000000000000000000A" + String(Math.floor(Math.random() * 10)),
      });
      const list = (await qa.bridge.call(request("sites.list"))) as { sites: unknown[] };
      sites = list.sites.length;
      const svc = (await qa.bridge.call(request("services.status"))) as { instances: unknown[] };
      instances = svc.instances.length;
      await qa.bridge.call(request(m));
    } catch (error) {
      const e = error as { code?: string; reason?: string };
      rejection = { code: e.code ?? "?", reason: e.reason ?? null };
    }
    return {
      sentinel: qa.sentinel,
      scenario: qa.scenario,
      state,
      hello: state.hello ?? null,
      sites,
      instances,
      rejection,
    };
  }, method);
}

for (const scenario of SCENARIOS) {
  test(`scenario ${scenario} opens without console errors`, async ({ page }) => {
    await page.goto(qaUrl({ scenario }));
    await expect(page.locator("main h1")).toBeVisible();
    const handle = await page.evaluate(() => {
      const qa = (window as unknown as { __FORGEBOX_QA__?: { scenario: string } }).__FORGEBOX_QA__;
      return qa?.scenario ?? null;
    });
    expect(handle).toBe(scenario);
  });
}

const EXPECT: Record<Scenario, (p: Probe) => void> = {
  empty: (p) => {
    expect(p.state.status).toBe("connected");
    expect(p.sites).toBe(0);
  },
  populated: (p) => {
    expect(p.sites).toBe(3);
    expect(p.instances).toBe(3);
  },
  heavy: (p) => {
    expect(p.sites).toBe(500);
    expect(p.instances).toBe(50);
  },
  errors: (p) => {
    expect(p.rejection?.code).toBe("internal");
    expect(p.rejection?.reason).toBe("fake_error");
  },
  "not-implemented": (p) => {
    expect(p.rejection?.code).toBe("not_implemented");
  },
  "daemon-down": (p) => {
    expect(p.state.status).toBe("stopped");
    expect(p.rejection?.code).toBe("disconnected");
  },
  slow: (p) => {
    expect(p.sites).toBe(3);
  },
  remote: (p) => {
    expect(p.hello?.local).toBe(false);
  },
};

for (const scenario of SCENARIOS) {
  test(`scenario ${scenario} behaves as scripted`, async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto(qaUrl({ scenario }));
    await expect(page.locator("main h1")).toBeVisible();
    const method = scenario === "remote" ? "services.config" : "tools.list";
    const result = await probe(page, method);
    expect(result.sentinel).toMatch(/^FORGEBOX_QA_SENTINEL_/);
    EXPECT[scenario](result);
    if (scenario === "remote") expect(result.rejection?.code).toBe("forbidden");
  });
}
