import { describe, expect, it } from "vitest";
import { createFormatters } from "../../../i18n/format";
import en from "../../../i18n/en-US/dashboard.json";
import pt from "../../../i18n/pt-BR/dashboard.json";
import { ACTIVITY_KINDS, humanizeEvent } from "./humanizeEvent";

const NOW = Date.parse("2026-11-03T12:00:00Z");

function sentence(bundle: typeof en, key: string, values: Record<string, string | number>): string {
  const events = bundle.activity.event as Record<string, string>;
  const template = events[key];
  if (template === undefined) throw new Error(`missing activity.event.${key}`);
  return template.replace(/\{(\w+)\}/g, (_, name: string) => String(values[name]));
}

describe("humanizeEvent", () => {
  const installed = {
    kind: "tool.install.completed",
    payload: { v: 1, tool: "php", version: "8.3.37", job_id: null },
  };

  it("names the tool and version, in both locales", () => {
    const human = humanizeEvent(installed, { toolName: (id) => id.toUpperCase() });
    expect(human).not.toBeNull();
    if (human === null) return;
    expect(human.key).toBe("tool_install_completed");
    expect(human.tone).toBe("success");
    expect(sentence(en, human.key, human.values)).toBe("PHP 8.3.37 installed");
    expect(sentence(pt, human.key, human.values)).toBe("PHP 8.3.37 instalado");
  });

  it("composes with the relative time of the formatters (2 min ago)", () => {
    const ts = NOW - 2 * 60 * 1000;
    expect(createFormatters("en-US").relative(ts, NOW)).toBe("2 min ago");
    expect(createFormatters("pt-BR").relative(ts, NOW)).toBe("há 2 min");
  });

  it("falls back to the tool id and drops noisy kinds", () => {
    expect(humanizeEvent(installed)?.values).toMatchObject({ tool: "php" });
    expect(humanizeEvent({ kind: "job.progress", payload: {} })).toBeNull();
    expect(humanizeEvent({ kind: "service.metrics", payload: {} })).toBeNull();
    expect(humanizeEvent({ kind: "tool.install.completed", payload: null })?.values).toEqual({
      tool: "",
      version: "",
    });
  });

  it("has a sentence in both locales for every kind it asks the daemon for", () => {
    for (const kind of ACTIVITY_KINDS) {
      const human = humanizeEvent({ kind, payload: {} });
      expect(human, kind).not.toBeNull();
      const key = human?.key ?? "";
      const events = (bundle: typeof en) => Object.keys(bundle.activity.event);
      const present = (bundle: typeof en) =>
        events(bundle).some((name) => name === key || name.startsWith(`${key}_`));
      expect(present(en), `en-US ${key}`).toBe(true);
      expect(present(pt), `pt-BR ${key}`).toBe(true);
    }
  });
});
