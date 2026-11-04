// Pure helpers of Settings > Seguranca (docs/11 sections 1 and 3): certificate state and the
// helper log lines. The clock is a parameter so the tests do not depend on today's date.
import type { CertificateDto } from "../../../../ipc/generated";

/** Days before `not_after` from which a certificate is shown as expiring (docs/07 5.2). */
export const EXPIRING_DAYS = 30;
const DAY_MS = 86_400_000;

export type CertState = "expired" | "expiring" | "untrusted" | "ok";

/** Whole days left (negative: expired). */
export function daysLeft(notAfter: string, now: Date): number {
  return Math.floor((Date.parse(notAfter) - now.getTime()) / DAY_MS);
}

/**
 * Expired wins, then expiring soon, then "the browser would not trust it" (issued by a CA that
 * was rotated away, or the CA is not trusted: `trusted === false`); an unchecked certificate
 * (`trusted` absent) is not called untrusted.
 */
export function certState(cert: CertificateDto, now: Date): CertState {
  const left = daysLeft(cert.not_after, now);
  if (!Number.isFinite(left) || left < 0) return "expired";
  if (left < EXPIRING_DAYS) return "expiring";
  if (cert.trusted === false) return "untrusted";
  return "ok";
}

export interface HelperLine {
  raw: string;
  /** Parsed `{ts, op, ok, code, message}`; `null` when the line is not JSON. */
  entry: { ts: string; op: string; ok: boolean; message: string } | null;
}

/** One line of `logs/helper.log` (JSON lines: ts, request_id, op, dry_run, ok, code, message). */
export function parseHelperLine(raw: string): HelperLine {
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value === "object" && value !== null) {
      const record = value as Record<string, unknown>;
      const text = (key: string) => (typeof record[key] === "string" ? record[key] : "");
      return {
        raw,
        entry: {
          ts: text("ts"),
          op: text("op"),
          ok: record.ok === true,
          message: text("message") || text("code"),
        },
      };
    }
  } catch {
    // Not JSON: shown as it is.
  }
  return { raw, entry: null };
}
