import { describe, expect, it } from "vitest";
import { formatPortSpec, normalizeTld, parsePortSpec, rangesOf, sameSpec } from "./model";

describe("normalizeTld", () => {
  it("accepts a dotted lowercase TLD and lowercases the rest", () => {
    expect(normalizeTld(".test")).toBe(".test");
    expect(normalizeTld("  .Dev ")).toBe(".dev");
    expect(normalizeTld(".my-tld1")).toBe(".my-tld1");
  });
  it("refuses a missing dot, subdomains, empty labels and edge hyphens", () => {
    for (const bad of ["test", ".", ".a.b", ".-x", ".x-", "", ". test"]) {
      expect(normalizeTld(bad), bad).toBeNull();
    }
  });
});

describe("parsePortSpec", () => {
  it("keeps single ports as numbers and spans as strings", () => {
    expect(parsePortSpec("80, 443")).toEqual({ ok: true, value: [80, 443] });
    expect(parsePortSpec("3306-3399,5432 - 5499")).toEqual({
      ok: true,
      value: ["3306-3399", "5432-5499"],
    });
    expect(parsePortSpec("9003-9003")).toEqual({ ok: true, value: [9003] });
  });
  it("refuses empty, zero, too large, reversed and malformed items", () => {
    for (const bad of ["", " , ", "0", "65536", "90-80", "abc", "80;81", "1-70000", "-5"]) {
      expect(parsePortSpec(bad).ok, bad).toBe(false);
    }
  });
});

describe("ranges", () => {
  it("formats the daemon value as the field text and lists the ranges by name", () => {
    expect(formatPortSpec([80, 443, "3306-3399"])).toBe("80, 443, 3306-3399");
    expect(formatPortSpec(null)).toBe("");
    const values = { ports: { ranges: { web: [80, 443], apps: ["3000-3999"] } } };
    expect(rangesOf(values)).toEqual([
      { name: "apps", spec: "3000-3999" },
      { name: "web", spec: "80, 443" },
    ]);
    expect(rangesOf({})).toEqual([]);
  });
  it("compares specs ignoring spacing", () => {
    expect(sameSpec("80,443", "80, 443")).toBe(true);
    expect(sameSpec("80", "81")).toBe(false);
    expect(sameSpec("x", "x")).toBe(false);
  });
});
