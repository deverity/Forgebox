import { describe, expect, it } from "vitest";
import type { CertificateDto } from "../../../../ipc/generated";
import { certState, daysLeft, parseHelperLine } from "./model";

const NOW = new Date("2026-11-04T12:00:00Z");
const DAY = 86_400_000;

function cert(daysFromNow: number, patch: Partial<CertificateDto> = {}): CertificateDto {
  return {
    host: "loja.test",
    sans: ["loja.test"],
    site_id: null,
    not_before: "2026-01-01T00:00:00Z",
    not_after: new Date(NOW.getTime() + daysFromNow * DAY).toISOString(),
    thumbprint_sha256: "AA",
    ...patch,
  };
}

describe("certState", () => {
  it("orders expired, expiring (< 30 days), untrusted and ok", () => {
    expect(certState(cert(-1), NOW)).toBe("expired");
    expect(certState(cert(29, { trusted: false }), NOW)).toBe("expiring");
    expect(certState(cert(30, { trusted: false }), NOW)).toBe("untrusted");
    expect(certState(cert(30), NOW)).toBe("ok");
    expect(certState(cert(200, { trusted: true }), NOW)).toBe("ok");
  });
  it("treats an unreadable date as expired", () => {
    expect(certState(cert(10, { not_after: "soon" }), NOW)).toBe("expired");
  });
  it("counts whole days", () => {
    expect(daysLeft(cert(10).not_after, NOW)).toBe(10);
  });
});

describe("parseHelperLine", () => {
  it("reads the JSON line of the helper log", () => {
    const line = parseHelperLine(
      JSON.stringify({ ts: "t", op: "dns.nrpt_apply", ok: false, code: "forbidden", message: "" }),
    );
    expect(line.entry).toEqual({ ts: "t", op: "dns.nrpt_apply", ok: false, message: "forbidden" });
  });
  it("keeps a line that is not JSON as it is", () => {
    expect(parseHelperLine("plain text")).toEqual({ raw: "plain text", entry: null });
    expect(parseHelperLine("42").entry).toBeNull();
  });
});
