// Pure model of the log viewer (M2-E3-T12): merging several streams into one ordered list,
// the 20,000-line cap, client-side filters, PHP stack-trace links and the CLI hint.
import { renderCli } from "../../ipc/cli";
import type { AccessLineDto, LogLineDto } from "../../ipc/generated";

/** Most rows the viewer keeps (the oldest are dropped first). */
export const MAX_ROWS = 20_000;
/** Most targets that can be followed together. */
export const MAX_TARGETS = 8;
/** Lines each target sends first (`logs.subscribe {tail}`) and each "load earlier" page. */
export const TAIL_LINES = 200;

/** One row of the list: a log line of a target, or the marker of lines the stream dropped. */
export type LogRow =
  | {
      kind: "line";
      /** Unique per viewer, grows with arrival; the React key and the selection key. */
      id: number;
      target: string;
      line: LogLineDto;
      /** Sort key: the line's `ts` in ms, or the previous key of its target when it has none. */
      at: number;
    }
  | { kind: "gap"; id: number; target: string; missed: number; at: number };

/** By time key; inside one target by `seq`; otherwise by arrival. */
function compare(a: LogRow, b: LogRow): number {
  if (a.at !== b.at) return a.at - b.at;
  if (a.kind === "line" && b.kind === "line" && a.target === b.target) {
    return a.line.seq - b.line.seq;
  }
  return a.id - b.id;
}

function keyOf(line: LogLineDto, fallback: number): number {
  if (line.ts !== null) {
    const parsed = Date.parse(line.ts);
    if (!Number.isNaN(parsed)) return parsed;
  }
  return fallback;
}

/**
 * Merges the lines of several targets. Rows are ordered by `ts`; a live line without `ts` takes the
 * larger of the previous key of its own target and the key of the newest row, so it stays where
 * it arrived (arrival order, even among targets with no `ts`). Ties break by arrival.
 */
export class LineMerger {
  private list: LogRow[] = [];
  private nextId = 1;
  private readonly lastAt = new Map<string, number>();
  private readonly newest = new Map<string, number>();

  constructor(private readonly cap = MAX_ROWS) {}

  get rows(): readonly LogRow[] {
    return this.list;
  }

  get size(): number {
    return this.list.length;
  }

  clear(): void {
    this.list = [];
    this.lastAt.clear();
    this.newest.clear();
  }

  /** Forgets a target and drops its rows (it was unselected). */
  remove(target: string): void {
    this.list = this.list.filter((row) => row.target !== target);
    this.lastAt.delete(target);
    this.newest.delete(target);
  }

  /** Lines that arrived live (or the first tail). Lines at or below the newest `seq` are skipped. */
  push(target: string, lines: readonly LogLineDto[]): number {
    const fresh: LogRow[] = [];
    // A line without ts never sorts before what is already shown: it takes the larger of its
    // source's previous key and the key of the newest row when the batch arrives.
    const floor = Math.max(this.lastAt.get(target) ?? 0, this.list[this.list.length - 1]?.at ?? 0);
    let at = floor;
    let top = this.newest.get(target) ?? -1;
    for (const line of lines) {
      if (line.seq <= top) continue;
      top = line.seq;
      at = keyOf(line, Math.max(at, floor));
      fresh.push({ kind: "line", id: this.nextId++, target, line, at });
    }
    this.lastAt.set(target, at);
    this.newest.set(target, top);
    this.insert(fresh);
    return fresh.length;
  }

  /** The "N lines lost" marker, placed where the target's stream stands. */
  gap(target: string, missed: number): void {
    const at = this.lastAt.get(target) ?? this.list[this.list.length - 1]?.at ?? 0;
    this.insert([{ kind: "gap", id: this.nextId++, target, missed, at }]);
  }

  /**
   * Older lines of a target (`logs.read {before_seq}`): only those below the oldest `seq` it
   * has. Returns how many were new.
   */
  prepend(target: string, lines: readonly LogLineDto[]): number {
    const oldest = this.oldestSeq(target);
    const first = this.list.find((row) => row.kind === "line" && row.target === target);
    let at = first?.at ?? 0;
    const fresh: LogRow[] = [];
    for (const line of [...lines].sort((a, b) => a.seq - b.seq)) {
      if (oldest !== null && line.seq >= oldest) continue;
      at = keyOf(line, at);
      fresh.push({ kind: "line", id: this.nextId++, target, line, at });
    }
    if (first === undefined) {
      this.newest.set(target, Math.max(-1, ...lines.map((line) => line.seq)));
      this.lastAt.set(target, fresh[fresh.length - 1]?.at ?? 0);
    }
    this.insert(fresh, true);
    return fresh.length;
  }

  /** `seq` of the oldest line the target still has, or null. */
  oldestSeq(target: string): number | null {
    let oldest: number | null = null;
    for (const row of this.list) {
      if (row.kind === "line" && row.target === target) {
        if (oldest === null || row.line.seq < oldest) oldest = row.line.seq;
      }
    }
    return oldest;
  }

  private insert(fresh: LogRow[], older = false): void {
    if (fresh.length === 0) return;
    const last = this.list[this.list.length - 1];
    let ordered = true;
    let before = last;
    for (const row of fresh) {
      if (before !== undefined && compare(before, row) > 0) ordered = false;
      before = row;
    }
    if (!older && ordered) {
      for (const row of fresh) this.list.push(row);
    } else {
      this.list = this.list.concat(fresh).sort(compare);
    }
    // Older pages that no longer fit fall off the front, like everything else.
    if (this.list.length > this.cap) this.list.splice(0, this.list.length - this.cap);
  }
}

// --- filters --------------------------------------------------------------------------------

export type LevelFilter = "all" | "error" | "warn" | "info" | "debug";
export type SinceFilter = "all" | "5m" | "15m" | "1h";
export const LEVELS: readonly LevelFilter[] = ["all", "error", "warn", "info", "debug"];
export const SINCE_OPTIONS: readonly SinceFilter[] = ["all", "5m", "15m", "1h"];
const SINCE_MS: Record<SinceFilter, number> = {
  all: 0,
  "5m": 5 * 60_000,
  "15m": 15 * 60_000,
  "1h": 3_600_000,
};

export interface Filters {
  text: string;
  level: LevelFilter;
  /** Access-log host, or "all". */
  site: string;
  since: SinceFilter;
}

export const NO_FILTERS: Filters = { text: "", level: "all", site: "all", since: "all" };

export function filtersActive(filters: Filters): boolean {
  return (
    filters.text.trim() !== "" ||
    filters.level !== "all" ||
    filters.site !== "all" ||
    filters.since !== "all"
  );
}

/** Canonical level of a line (`warning` -> `warn`; `notice` -> `info`; unknown -> null). */
export function levelOf(line: LogLineDto): Exclude<LevelFilter, "all"> | null {
  const raw = (line.level ?? "").toLowerCase();
  if (["error", "err", "fatal", "critical", "crit", "alert", "emerg"].includes(raw)) return "error";
  if (["warn", "warning"].includes(raw)) return "warn";
  if (["info", "notice"].includes(raw)) return "info";
  if (["debug", "trace"].includes(raw)) return "debug";
  return null;
}

/** Rows that pass the filters. Gap markers always pass: lost lines are never hidden. */
export function applyFilters(
  rows: readonly LogRow[],
  filters: Filters,
  now: number,
): readonly LogRow[] {
  if (!filtersActive(filters)) return rows;
  const needle = filters.text.trim().toLowerCase();
  const horizon = filters.since === "all" ? null : now - SINCE_MS[filters.since];
  return rows.filter((row) => {
    if (row.kind === "gap") return true;
    const { line } = row;
    if (needle !== "" && !line.text.toLowerCase().includes(needle)) return false;
    if (filters.level !== "all" && levelOf(line) !== filters.level) return false;
    if (filters.site !== "all" && line.access?.host !== filters.site) return false;
    // Lines with no usable `ts` cannot be placed in time: they stay.
    if (horizon !== null && line.ts !== null) {
      const at = Date.parse(line.ts);
      if (!Number.isNaN(at) && at < horizon) return false;
    }
    return true;
  });
}

/** Hosts seen in access lines (the "site" filter options), sorted. */
export function accessHosts(rows: readonly LogRow[]): string[] {
  const hosts = new Set<string>();
  for (const row of rows) {
    if (row.kind === "line" && row.line.access) hosts.add(row.line.access.host);
  }
  return [...hosts].sort();
}

// --- text: highlight and PHP file links ----------------------------------------------------

export interface FileLink {
  path: string;
  line: number;
}

export interface TextPart {
  text: string;
  /** The part matches the text filter. */
  match?: boolean;
  /** The part is a `file:line` of a stack trace. */
  link?: FileLink;
}

/**
 * Paths of PHP files with a line: `C:\www\a.php:12`, `/var/www/a.php(12)` (PHP and Laravel
 * stack frames, `#3 file(12): call()`) and `/x/a.php on line 12`. Paths with spaces are not
 * recognised (the line stays plain text).
 */
const FILE_LINE =
  /((?:[A-Za-z]:[\\/]|[\\/])[^\s:()'"<>|*?]+?\.php)(?::(\d+)|\((\d+)\)| on line (\d+))/g;

export function findFileLinks(text: string): { start: number; end: number; link: FileLink }[] {
  const out: { start: number; end: number; link: FileLink }[] = [];
  for (const match of text.matchAll(FILE_LINE)) {
    const path = match[1];
    const line = Number(match[2] ?? match[3] ?? match[4]);
    if (path === undefined || !Number.isFinite(line) || line < 1) continue;
    out.push({ start: match.index, end: match.index + match[0].length, link: { path, line } });
  }
  return out;
}

function highlightInto(text: string, needle: string, out: TextPart[]): void {
  if (needle === "") {
    if (text !== "") out.push({ text });
    return;
  }
  const lower = text.toLowerCase();
  let from = 0;
  for (;;) {
    const at = lower.indexOf(needle, from);
    if (at < 0) break;
    if (at > from) out.push({ text: text.slice(from, at) });
    out.push({ text: text.slice(at, at + needle.length), match: true });
    from = at + needle.length;
  }
  if (from < text.length) out.push({ text: text.slice(from) });
}

/** Splits a line into plain, highlighted and link parts (`links` off: remote daemon). */
export function splitText(text: string, filter: string, links: boolean): TextPart[] {
  const needle = filter.trim().toLowerCase();
  const found = links ? findFileLinks(text) : [];
  const parts: TextPart[] = [];
  let from = 0;
  for (const hit of found) {
    highlightInto(text.slice(from, hit.start), needle, parts);
    parts.push({ text: text.slice(hit.start, hit.end), link: hit.link });
    from = hit.end;
  }
  highlightInto(text.slice(from), needle, parts);
  return parts;
}

// --- misc ----------------------------------------------------------------------------------

/** The CLI line that follows the same target: `forge logs nginx.error -f --grep upstream`. */
export function cliHint(target: string, grep: string): string | null {
  const text = grep.trim();
  return renderCli("logs.subscribe", text === "" ? { target } : { target, grep: text });
}

/** Time of day of an ISO instant, or "" (access lines show it as a column). */
export function clock(ts: string | null): string {
  if (ts === null) return "";
  const match = /T(\d{2}:\d{2}:\d{2})/.exec(ts);
  return match?.[1] ?? ts;
}

export function statusTone(access: AccessLineDto): "ok" | "warn" | "error" {
  if (access.status >= 500) return "error";
  if (access.status >= 400) return "warn";
  return "ok";
}
