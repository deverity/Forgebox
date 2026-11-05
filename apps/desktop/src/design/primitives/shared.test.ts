import { describe, expect, it } from "vitest";
import { FIELD_BASE, conflictingColorClasses } from "./shared";

describe("conflictingColorClasses", () => {
  it("reports two colours of the same property", () => {
    expect(conflictingColorClasses("border border-stroke-strong border-state-failed")).toEqual([
      "border-stroke-strong",
      "border-state-failed",
    ]);
  });

  it("ignores different properties and different variants", () => {
    expect(
      conflictingColorClasses(
        "border-stroke-strong hover:border-accent bg-surface text-fg-primary",
      ),
    ).toEqual([]);
  });

  it("FIELD_BASE carries no border colour", () => {
    expect(FIELD_BASE).not.toMatch(/border-(?:stroke|state|accent)/);
  });
});
