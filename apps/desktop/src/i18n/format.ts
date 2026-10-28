/** Locale-aware formatters built on `Intl` (docs/07 §9). Numbers in the UI also get `TNUM_CLASS`. */

/** Tailwind class enabling tabular figures (`font-feature-settings: "tnum"`). */
export const TNUM_CLASS = "tabular-nums";

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

export interface Formatters {
  number(value: number, options?: Intl.NumberFormatOptions): string;
  /** Binary-scaled size: `410 MB`, `1,5 GB`. */
  bytes(value: number): string;
  /** `fraction` is 0..1: `0.255` becomes `25,5%`. */
  percent(fraction: number, maxFractionDigits?: number): string;
  /** Compact duration from milliseconds: `2h 14m`, `45s`, `350ms`. */
  duration(ms: number): string;
  /** Relative to `now`: `há 2 min` / `2 min ago`. */
  relative(date: Date | number, now?: Date | number): string;
  date(date: Date | number, options?: Intl.DateTimeFormatOptions): string;
}

function toMs(d: Date | number): number {
  return typeof d === "number" ? d : d.getTime();
}

export function createFormatters(locale: string): Formatters {
  // "pseudo" is not a BCP 47 tag; format like en-US so QA still sees realistic numbers.
  const tag = locale === "pseudo" ? "en-US" : locale;
  const number = (value: number, options?: Intl.NumberFormatOptions) =>
    new Intl.NumberFormat(tag, options).format(value);

  return {
    number,
    bytes(value) {
      let v = Math.abs(value);
      let unit = 0;
      while (v >= 1024 && unit < BYTE_UNITS.length - 1) {
        v /= 1024;
        unit += 1;
      }
      const signed = value < 0 ? -v : v;
      const digits = unit === 0 || v >= 100 ? 0 : v >= 10 ? 1 : 2;
      return `${number(signed, { maximumFractionDigits: digits })} ${BYTE_UNITS[unit] ?? "B"}`;
    },
    percent(fraction, maxFractionDigits = 1) {
      return number(fraction, { style: "percent", maximumFractionDigits: maxFractionDigits });
    },
    duration(ms) {
      const total = Math.max(0, Math.round(ms));
      if (total < 1000) return `${number(total)}ms`;
      const seconds = Math.floor(total / 1000);
      const d = Math.floor(seconds / 86400);
      const h = Math.floor((seconds % 86400) / 3600);
      const m = Math.floor((seconds % 3600) / 60);
      const s = seconds % 60;
      const parts: string[] = [];
      if (d > 0) parts.push(`${number(d)}d`, `${number(h)}h`);
      else if (h > 0) parts.push(`${number(h)}h`, `${number(m)}m`);
      else if (m > 0) parts.push(`${number(m)}m`, ...(s > 0 && m < 10 ? [`${number(s)}s`] : []));
      else parts.push(`${number(s)}s`);
      return parts.join(" ");
    },
    relative(date, now = Date.now()) {
      const diffSec = Math.round((toMs(date) - toMs(now)) / 1000);
      const abs = Math.abs(diffSec);
      const rtf = new Intl.RelativeTimeFormat(tag, { numeric: "auto", style: "short" });
      // ICU abbreviates with a trailing dot ("há 2 min."); the design uses "há 2 min".
      const fmt = (v: number, unit: Intl.RelativeTimeFormatUnit) =>
        rtf.format(v, unit).replace(/\.(?=\s|$)/g, "");
      if (abs < 45) return fmt(diffSec, "second");
      if (abs < 3600) return fmt(Math.round(diffSec / 60), "minute");
      if (abs < 86400) return fmt(Math.round(diffSec / 3600), "hour");
      return fmt(Math.round(diffSec / 86400), "day");
    },
    date(date, options = { dateStyle: "medium", timeStyle: "short" }) {
      return new Intl.DateTimeFormat(tag, options).format(date);
    },
  };
}
