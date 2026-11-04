import { describe, expect, it } from "vitest";
import { QA_CONFIG_SCHEMA } from "../../../qa/fixtures/scenarios/settings";
import {
  buildRows,
  encode,
  humanizeKey,
  matchesSearch,
  toDraft,
  valueAt,
  withKnownOptions,
  type RowSpec,
} from "./model";

const rows = buildRows({ schema: QA_CONFIG_SCHEMA });
const row = (key: string): RowSpec => {
  const found = rows.find((r) => r.key === key);
  if (found === undefined) throw new Error(`no row ${key}`);
  return found;
};

describe("buildRows", () => {
  it("maps schema nodes to the right controls", () => {
    expect(row("case_sensitive").control).toBe("toggle");
    expect(row("dns.enabled").control).toBe("toggle");
    expect(row("tld").control).toBe("text");
    expect(row("parked_dirs").control).toBe("list");
    expect(row("editor.command")).toMatchObject({ control: "text", nullable: true });
    expect(row("terminal.max_sessions")).toMatchObject({
      control: "number",
      integer: true,
      min: 1,
      max: 256,
    });
  });

  it("turns a oneOf of string consts behind a $ref into a select", () => {
    expect(row("updates.metered")).toMatchObject({
      control: "select",
      options: ["pause", "allow"],
      description: "What to do on a metered connection.",
    });
  });

  it("keeps field descriptions and humanizes missing titles", () => {
    expect(row("tld").description).toBe("TLD of local sites, with the leading dot.");
    expect(humanizeKey("terminal.integrated_shell")).toBe("Integrated shell");
  });

  it("skips what has no control instead of guessing", () => {
    const odd = buildRows({
      schema: {
        type: "object",
        properties: {
          ports: { type: "object", additionalProperties: { type: "string" } },
          mixed: { type: "array", items: { type: "integer" } },
          fine: { type: "boolean" },
        },
      },
    });
    expect(odd.map((r) => r.key)).toEqual(["fine"]);
    expect(buildRows({ schema: null })).toEqual([]);
  });

  it("offers the shells the integrated terminal knows plus 'unset'", () => {
    const shell = withKnownOptions(row("terminal.integrated_shell"));
    expect(shell).toMatchObject({ control: "select", options: ["", "pwsh", "powershell", "cmd"] });
  });
});

describe("values and encoding", () => {
  it("reads dotted keys and shows null as empty", () => {
    const values = { editor: { command: null }, dns: { enabled: true }, parked_dirs: ["a", "b"] };
    expect(valueAt(values, "dns.enabled")).toBe(true);
    expect(valueAt(values, "nope.deep")).toBeUndefined();
    expect(toDraft(row("editor.command"), valueAt(values, "editor.command"))).toBe("");
    expect(toDraft(row("parked_dirs"), valueAt(values, "parked_dirs"))).toBe("a, b");
  });

  it("encodes numbers with their range and unsets a nullable text on empty", () => {
    expect(encode(row("terminal.max_sessions"), "12")).toEqual({ ok: true, value: 12 });
    expect(encode(row("terminal.max_sessions"), "0")).toEqual({ ok: false, problem: "range" });
    expect(encode(row("terminal.max_sessions"), "1.5")).toEqual({ ok: false, problem: "number" });
    expect(encode(row("terminal.max_sessions"), "")).toEqual({ ok: false, problem: "number" });
    expect(encode(row("editor.command"), "  ")).toEqual({ ok: true, value: null });
    expect(encode(row("tld"), ".dev")).toEqual({ ok: true, value: ".dev" });
    expect(encode(row("parked_dirs"), "www, , D:/sites")).toEqual({
      ok: true,
      value: ["www", "D:/sites"],
    });
  });
});

describe("matchesSearch", () => {
  it("ignores case and accents", () => {
    expect(matchesSearch("config", "Configurações")).toBe(true);
    expect(matchesSearch("", "x")).toBe(true);
    expect(matchesSearch("zzz", "Editor")).toBe(false);
  });
});
