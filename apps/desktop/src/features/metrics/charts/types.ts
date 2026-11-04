// Input shapes of the metrics charts (M2-E5-T15). Columnar on purpose: uPlot takes parallel
// arrays, so the data layer (M2-E5-T14) can hand its series over without a per-point copy.
// `t` is unix seconds; `null` marks a gap (no traffic in that bucket).

export type Column = readonly (number | null)[];

export interface LatencySeries {
  t: readonly number[];
  /** Milliseconds. */
  p50: Column;
  p90: Column;
  p99: Column;
  /** Optional PHP-time line (ms), same length as `t`. */
  php?: Column;
}

export interface ThroughputSeries {
  t: readonly number[];
  /** Requests per second (total). */
  rps: Column;
  e4xx: Column;
  e5xx: Column;
}

export interface ResourceSeries {
  t: readonly number[];
  /** Percent of one core budget, 0..100. */
  cpu: Column;
  /** Bytes. */
  ram: Column;
}

/** docs/14 section 4.1: PHP version switch, restart, config change, update. */
export type MarkerKind = "php-version" | "restart" | "config" | "update";

export interface ChartMarker {
  id: string;
  /** Unix seconds. */
  t: number;
  kind: MarkerKind;
  /** Optional detail, e.g. "8.3 -> 8.4". */
  detail?: string;
}

export interface SummaryRow {
  label: string;
  latest: string;
  peak: string;
}
