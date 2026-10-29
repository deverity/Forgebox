import { afterEach, describe, expect, it } from "vitest";
import { QA_SCENARIO_NAMES } from "../../qa/fixtures/scenarios";
import { bootQa, QA_SENTINEL } from "./bootstrap";
import { qaLocaleOverride } from "./pseudo-locale";

afterEach(() => {
  window.__FORGEBOX_QA__?.stop();
  delete window.__FORGEBOX_QA__;
  delete document.documentElement.dataset.theme;
  delete document.documentElement.dataset.vibrancy;
});

describe("qa bootstrap", () => {
  it("applies the appearance parameters and publishes the handle", () => {
    const handle = bootQa("?scenario=populated&theme=dark&vibrancy=off&locale=pseudo&latency=7");
    expect(window.__FORGEBOX_QA__).toBe(handle);
    expect(handle.sentinel).toBe(QA_SENTINEL);
    expect(handle.scenario).toBe("populated");
    expect(handle.locale).toBe("pseudo");
    expect(handle.bridge.scenario.latencyMs).toBe(7);
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.dataset.vibrancy).toBe("off");
  });

  it("every scenario of the library boots and answers sites.list", async () => {
    for (const name of QA_SCENARIO_NAMES) {
      const handle = bootQa(`?scenario=${name}&latency=0`);
      expect(handle.scenario).toBe(name);
      handle.stop();
    }
    const heavy = bootQa("?scenario=heavy");
    const list = (await heavy.bridge.call({
      context: "local",
      method: "sites.list",
      params: {},
      request_id: "01JQA00000000000000000000A",
    })) as { sites: unknown[] };
    expect(list.sites).toHaveLength(500);
  });

  it("reduced motion makes the media query match", () => {
    const original = Object.getOwnPropertyDescriptor(window, "matchMedia");
    window.matchMedia = (query: string) =>
      ({
        matches: false,
        media: query,
        addEventListener: () => undefined,
      }) as unknown as MediaQueryList;
    try {
      bootQa("?motion=reduced");
      expect(window.matchMedia("(prefers-reduced-motion: reduce)").matches).toBe(true);
      expect(window.matchMedia("(min-width: 1px)").matches).toBe(false);
    } finally {
      if (original) Object.defineProperty(window, "matchMedia", original);
      else Reflect.deleteProperty(window, "matchMedia");
    }
  });

  it("maps locale values", () => {
    expect(qaLocaleOverride("PSEUDO")).toBe("pseudo");
    expect(qaLocaleOverride("pt-br")).toBe("pt-BR");
    expect(qaLocaleOverride("xx")).toBeNull();
    expect(qaLocaleOverride(null)).toBeNull();
  });
});
