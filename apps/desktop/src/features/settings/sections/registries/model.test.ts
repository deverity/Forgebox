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
  const base = { official: false, trusted_unsigned: false, verified: true };
  it("tells official, third-party and unverified apart", () => {
    expect(registryTrust({ ...base, official: true })).toBe("official");
    expect(registryTrust(base)).toBe("third-party");
    expect(registryTrust({ ...base, trusted_unsigned: true, verified: false })).toBe("unverified");
    expect(registryTrust({ ...base, verified: false })).toBe("unverified");
  });
  it("does not guess from the URL or the kind (a signed dir mirror is verified)", () => {
    // The input has no url/kind at all: only the signature state counts.
    expect(registryTrust(base)).toBe("third-party");
  });
});
