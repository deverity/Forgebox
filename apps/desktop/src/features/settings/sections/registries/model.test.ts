import { describe, expect, it } from "vitest";
import { registryTrust, urlProblem } from "./model";

describe("registry url validation", () => {
  it.each([
    "https://example.org/registry/index.yaml",
    "git+https://github.com/acme/registry.git",
    "http://localhost:8080/index.yaml",
    "file:///C:/registries/mine",
    "C:\\registries\\mine",
    "\\\\server\\share\\reg",
  ])("accepts %s", (value) => {
    expect(urlProblem(value)).toBeUndefined();
  });

  it("refuses an empty value, an unknown scheme and a malformed URL", () => {
    expect(urlProblem("   ")).toBe("empty");
    expect(urlProblem("ftp://example.org/x")).toBe("scheme");
    expect(urlProblem("not a url")).toBe("invalid");
    expect(urlProblem("https://")).toBe("invalid");
  });
});

describe("registry trust badge", () => {
  const base = { official: false, url: "https://example.org/index.yaml", kind: "http" as const };
  it("tells official, third-party and unverified apart", () => {
    expect(registryTrust({ ...base, official: true })).toBe("official");
    expect(registryTrust(base)).toBe("third-party");
    expect(registryTrust({ ...base, url: "http://example.org/index.yaml" })).toBe("unverified");
    expect(registryTrust({ ...base, kind: "dir", url: "C:/reg" })).toBe("unverified");
  });
});
