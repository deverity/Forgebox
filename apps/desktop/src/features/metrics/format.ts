// Locale-aware formatters for the metrics screens (docs/07 section 9): ms, req/s, percent,
// bytes, the "≈" prefix of apportioned values and tabular figures. Built on `Intl`, so a
// decimal comma in pt-BR and a point in en-US; unit text is locale-neutral. Missing values
// (`null`, `NaN`) render as an em dash, never as 0.
import { TNUM_CLASS, createFormatters } from "../../i18n/format";

export { TNUM_CLASS };

export const NO_VALUE = "—";
const NBSP = " ";
/** Prefix of CPU/RAM estimates of a shared pool (`approx`). */
export const APPROX = "≈";

export interface MetricsFormatters {
  /** Latency: `<1 ms`, `4,2 ms`, `230 ms`, `1,3 s`. */
  ms(value: number | null | undefined): string;
  /** Requests per second: `0,4 req/s`, `12 req/s`, `1.250 req/s`. */
  rps(value: number | null | undefined): string;
  /** A value already in percent (`error_pct`, `cpu_pct`): `0,8%`. */
  percent(value: number | null | undefined): string;
  bytes(value: number | null | undefined): string;
  /** Plain count with grouping. */
  count(value: number | null | undefined): string;
  /** Prefixes `≈` + no-break space when `approx` is true. */
  approx(text: string, approx: boolean | undefined): string;
  tnum: string;
}

function finite(value: number | null | undefined): value is number {
  return value !== null && value !== undefined && Number.isFinite(value);
}

export function createMetricsFormatters(locale: string): MetricsFormatters {
  const base = createFormatters(locale);
  const fixed = (value: number, digits: number) =>
    base.number(value, { minimumFractionDigits: 0, maximumFractionDigits: digits });
  return {
    ms(value) {
      if (!finite(value)) return NO_VALUE;
      const v = Math.max(0, value);
      if (v > 0 && v < 1) return `<1${NBSP}ms`;
      if (v < 10) return `${fixed(v, 1)}${NBSP}ms`;
      if (v < 1000) return `${fixed(Math.round(v), 0)}${NBSP}ms`;
      return `${fixed(v / 1000, 1)}${NBSP}s`;
    },
    rps(value) {
      if (!finite(value)) return NO_VALUE;
      const v = Math.max(0, value);
      const digits = v >= 10 ? 0 : v >= 1 ? 1 : 2;
      return `${fixed(v, digits)}${NBSP}req/s`;
    },
    percent(value) {
      if (!finite(value)) return NO_VALUE;
      const v = Math.max(0, value);
      return base.percent(v / 100, v >= 10 ? 0 : 1);
    },
    bytes(value) {
      return finite(value) ? base.bytes(value) : NO_VALUE;
    },
    count(value) {
      return finite(value) ? base.number(Math.round(value)) : NO_VALUE;
    },
    approx(text, approx) {
      return approx === true && text !== NO_VALUE ? `${APPROX}${NBSP}${text}` : text;
    },
    tnum: TNUM_CLASS,
  };
}
