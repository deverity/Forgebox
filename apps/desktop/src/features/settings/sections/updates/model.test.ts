import { describe, expect, it } from "vitest";
import { quietOf, quietProblem, quietValue } from "./model";

describe("quiet hours", () => {
  it("accepts a window, a wrapping window and an empty pair", () => {
    expect(quietProblem("22:00", "07:00")).toBeUndefined();
    expect(quietProblem("01:30", "05:00")).toBeUndefined();
    expect(quietProblem("", "")).toBeUndefined();
  });

  it("refuses bad times, a half pair and identical times", () => {
    expect(quietProblem("24:00", "07:00")).toBe("format");
    expect(quietProblem("7:00", "08:00")).toBe("format");
    expect(quietProblem("22:60", "07:00")).toBe("format");
    expect(quietProblem("22:00", "")).toBe("incomplete");
    expect(quietProblem("", "07:00")).toBe("incomplete");
    expect(quietProblem("08:00", "08:00")).toBe("same");
  });

  it("saves null for no quiet hours and trims the pair", () => {
    expect(quietValue("", "")).toBeNull();
    expect(quietValue(" 22:00", "07:00 ")).toEqual({ start: "22:00", end: "07:00" });
  });

  it("reads the document", () => {
    expect(quietOf({ updates: { quiet_hours: { start: "22:00", end: "07:00" } } })).toEqual({
      start: "22:00",
      end: "07:00",
    });
    expect(quietOf({ updates: {} })).toEqual({ start: "", end: "" });
    expect(quietOf({ updates: { quiet_hours: null } })).toEqual({ start: "", end: "" });
  });
});
