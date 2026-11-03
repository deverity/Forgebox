import { describe, expect, it } from "vitest";
import type { QuickSettingDto } from "../../../ipc/generated";
import {
  applyOf,
  baselineOf,
  changedSettings,
  dirtyKeys,
  markManagedLines,
  resetTarget,
  validateSetting,
  valueText,
} from "./model";

function setting(
  patch: Partial<QuickSettingDto> & Pick<QuickSettingDto, "key" | "kind">,
): QuickSettingDto {
  return { apply: "reload", ...patch };
}

describe("valueText", () => {
  it("writes each value the way services.set takes it", () => {
    expect(valueText(true)).toBe("true");
    expect(valueText(512)).toBe("512");
    expect(valueText("1G")).toBe("1G");
    expect(valueText(["a", "b"])).toBe("a,b");
    expect(valueText(null)).toBe("");
  });
});

describe("validateSetting", () => {
  it("checks numbers and their bounds", () => {
    const number = setting({ key: "n", kind: "number", min: 10, max: 100 });
    expect(validateSetting(number, "abc")).toEqual({ code: "number" });
    expect(validateSetting(number, "5")).toEqual({ code: "range", min: 10, max: 100 });
    expect(validateSetting(number, "101")).toEqual({ code: "range", min: 10, max: 100 });
    expect(validateSetting(number, "50")).toBeUndefined();
  });

  it("checks ports, options and multi items", () => {
    expect(validateSetting(setting({ key: "p", kind: "port" }), "70000")).toEqual({ code: "port" });
    expect(validateSetting(setting({ key: "p", kind: "port" }), "9003")).toBeUndefined();
    const select = setting({ key: "s", kind: "select", options: ["a", "b"] });
    expect(validateSetting(select, "c")).toEqual({ code: "option" });
    expect(validateSetting(select, "b")).toBeUndefined();
    const multi = setting({ key: "m", kind: "multi", options: ["a", "b"] });
    expect(validateSetting(multi, "a,x")).toEqual({ code: "option" });
    expect(validateSetting(multi, "")).toBeUndefined();
    expect(validateSetting(multi, "a,b")).toBeUndefined();
  });

  it("refuses control characters in text and paths", () => {
    expect(validateSetting(setting({ key: "t", kind: "text" }), "a\nb")).toEqual({ code: "path" });
    expect(validateSetting(setting({ key: "t", kind: "path" }), "C:/tmp")).toBeUndefined();
  });
});

describe("dirty tracking", () => {
  const settings = [
    setting({ key: "a", kind: "text", value: "x", apply: "none" }),
    setting({ key: "b", kind: "number", value: 1, apply: "reload" }),
  ];
  const baseline = baselineOf(settings, [{ name: "main", port: 3306, protocol: "tcp" }]);

  it("sends only the changed keys, ports included", () => {
    expect(dirtyKeys(baseline, baseline)).toEqual([]);
    const draft = { ...baseline, b: "2", "ports.main": "3307" };
    expect(changedSettings(baseline, draft)).toEqual({ b: "2", "ports.main": "3307" });
  });

  it("tells reload from restart: a port change or a restart setting restarts", () => {
    const withRestart = [...settings, setting({ key: "c", kind: "text", apply: "restart" })];
    expect(applyOf(withRestart, [])).toBeNull();
    expect(applyOf(withRestart, ["a"])).toBe("none");
    expect(applyOf(withRestart, ["a", "b"])).toBe("reload");
    expect(applyOf(withRestart, ["b", "c"])).toBe("restart");
    expect(applyOf(withRestart, ["ports.main"])).toBe("restart");
  });

  it("resets to the saved value first, then to the default", () => {
    const field = setting({ key: "a", kind: "text", value: "x", default: "d" });
    expect(resetTarget(field, { a: "x" }, { a: "y" })).toEqual({ kind: "undo", text: "x" });
    expect(resetTarget(field, { a: "x" }, { a: "x" })).toEqual({ kind: "default", text: "d" });
    expect(resetTarget({ ...field, default: "x" }, { a: "x" }, { a: "x" })).toBeNull();
  });
});

describe("markManagedLines", () => {
  it("marks the block and its markers, for #, ; and // comments", () => {
    const lines = markManagedLines(
      [
        "a",
        "# forgebox:begin x",
        "b",
        "# forgebox:end x",
        "c",
        "; forgebox:begin y",
        "d",
        "; forgebox:end y",
      ].join("\n"),
    );
    expect(lines.map((l) => l.managed)).toEqual([false, true, true, true, false, true, true, true]);
  });
});
