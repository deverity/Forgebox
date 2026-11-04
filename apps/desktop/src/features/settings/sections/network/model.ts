// Pure helpers of Settings > Rede (docs/07 section 5.11, docs/06 section 5): the TLD check and
// the port range text. The daemon stays the validator (`config_invalid`); this only keeps an
// obviously wrong value from being sent.
import { valueAt } from "../../model";

/** A TLD as `forgebox.toml` stores it: leading dot, lowercase, letters/digits/hyphen labels. */
export function normalizeTld(text: string): string | null {
  const value = text.trim().toLowerCase();
  return /^\.[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(value) ? value : null;
}

export type PortItem = number | string;

export type PortSpecResult = { ok: true; value: PortItem[] } | { ok: false };

const MAX_PORT = 65535;

function portOf(text: string): number | null {
  if (!/^\d{1,5}$/.test(text)) return null;
  const port = Number(text);
  return port >= 1 && port <= MAX_PORT ? port : null;
}

/**
 * `"80, 443, 3306-3399"` to the JSON `forgebox.toml` keeps: a single port is a number, a span
 * the string `"start-end"` (the shape `PortSet` serializes). Empty, 0, above 65535 and
 * `start > end` are refused.
 */
export function parsePortSpec(text: string): PortSpecResult {
  const parts = text
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  if (parts.length === 0) return { ok: false };
  const value: PortItem[] = [];
  for (const part of parts) {
    const span = /^(\d+)\s*-\s*(\d+)$/.exec(part);
    if (span !== null) {
      const start = portOf(span[1] ?? "");
      const end = portOf(span[2] ?? "");
      if (start === null || end === null || start > end) return { ok: false };
      value.push(start === end ? start : `${String(start)}-${String(end)}`);
      continue;
    }
    const port = portOf(part);
    if (port === null) return { ok: false };
    value.push(port);
  }
  return { ok: true, value };
}

/** The text a range shows: `"80, 443"`, `"3306-3399, 5432-5499"`. */
export function formatPortSpec(value: unknown): string {
  if (!Array.isArray(value)) return "";
  return value
    .filter((item): item is PortItem => typeof item === "number" || typeof item === "string")
    .map(String)
    .join(", ");
}

export interface RangeEntry {
  name: string;
  /** Effective value, as the text of the field. */
  spec: string;
}

/** Ranges of `ports.ranges` in the order the daemon lists them (alphabetical by name). */
export function rangesOf(values: unknown): RangeEntry[] {
  const ranges = valueAt(values, "ports.ranges");
  if (typeof ranges !== "object" || ranges === null || Array.isArray(ranges)) return [];
  return Object.entries(ranges as Record<string, unknown>)
    .map(([name, value]) => ({ name, spec: formatPortSpec(value) }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** Same ports as text, ignoring spacing (`"80,443"` equals `"80, 443"`). */
export function sameSpec(a: string, b: string): boolean {
  const left = parsePortSpec(a);
  const right = parsePortSpec(b);
  return left.ok && right.ok && JSON.stringify(left.value) === JSON.stringify(right.value);
}
