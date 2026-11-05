// Pure helpers of the Sites screen: the search index, the host suggestion and the mapping of
// an `invalid_input` rejection to a form field. No React, no IPC.
import type { ForgeApiError } from "../../ipc/errors";
import type * as T from "../../ipc/generated";

/** Folds case and accents so "Café" finds "cafe" (pt-BR names). */
export function fold(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

export interface SiteIndexEntry {
  site: T.SiteDto;
  /** Folded host, aliases, root and driver, joined by a newline. */
  haystack: string;
}

/** Sorted by host, one folded haystack per site: a keystroke only runs `includes`. */
export function indexSites(sites: Readonly<Record<string, T.SiteDto>>): SiteIndexEntry[] {
  return Object.values(sites)
    .sort((a, b) => (a.host < b.host ? -1 : a.host > b.host ? 1 : 0))
    .map((site) => ({
      site,
      haystack: fold([site.host, ...site.aliases, site.root ?? "", site.driver ?? ""].join("\n")),
    }));
}

/** Every word of `query` must appear in the host, an alias, the root or the driver. */
export function filterSites(entries: readonly SiteIndexEntry[], query: string): SiteIndexEntry[] {
  const words = fold(query)
    .split(/\s+/)
    .filter((word) => word !== "");
  if (words.length === 0) return [...entries];
  return entries.filter((entry) => words.every((word) => entry.haystack.includes(word)));
}

/** Last path segment of a Windows or POSIX path. */
export function folderName(path: string): string {
  const parts = path.split(/[\\/]+/).filter((part) => part !== "");
  return parts.at(-1) ?? "";
}

/** `C:\Meus Projetos\Loja Nova` with tld `test` gives `loja-nova.test`; empty if nothing usable. */
export function suggestHost(path: string, tld: string): string {
  const label = fold(folderName(path))
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 63)
    .replace(/-+$/g, "");
  const suffix = tld.replace(/^\.+/, "").toLowerCase();
  if (label === "") return "";
  return suffix === "" ? label : `${label}.${suffix}`;
}

/** Hosts are always lowercase (docs/15 section 7): the daemon stores `loja.test` for `Loja.Test`. */
export function normalizeHost(input: string): { host: string; lowercased: boolean } {
  const trimmed = input.trim();
  const host = trimmed.toLowerCase();
  return { host, lowercased: host !== trimmed };
}

export type SiteField = "dir" | "host" | "upstream";

const FIELDS: readonly SiteField[] = ["dir", "host", "upstream"];

/**
 * The form field an `invalid_input` rejection is about: `details.args.field` when the daemon
 * names it, otherwise guessed from the reason (a host outside the configured TLD, a bad
 * upstream URL, a missing folder). `null` when the error is not about one field.
 */
export function fieldOfInvalidInput(error: ForgeApiError, fallback: SiteField): SiteField | null {
  if (error.code !== "invalid_input") return null;
  const named = error.args.field;
  if (typeof named === "string") {
    const found = FIELDS.find((field) => field === named);
    if (found !== undefined) return found;
  }
  const reason = `${error.reason ?? ""} ${error.message}`.toLowerCase();
  if (/upstream|url/.test(reason)) return "upstream";
  if (/tld|host|alias|label/.test(reason)) return "host";
  if (/(^|[^a-z])(dir|folder|path|root)([^a-z]|$)/.test(reason)) return "dir";
  return fallback;
}

/** Host form validation before sending (the daemon validates again). */
export function hostProblem(host: string): "empty" | "syntax" | null {
  if (host === "") return "empty";
  if (!/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$/.test(host)) {
    return "syntax";
  }
  return null;
}

export function upstreamProblem(upstream: string): "empty" | "syntax" | null {
  if (upstream.trim() === "") return "empty";
  try {
    const url = new URL(upstream.trim());
    return url.protocol === "http:" || url.protocol === "https:" ? null : "syntax";
  } catch {
    return "syntax";
  }
}

/** Series (`8.4`) of the installed PHP versions, newest first. */
export function phpSeries(tool: T.ToolDto | undefined): string[] {
  if (tool === undefined) return [];
  const series = new Set<string>();
  for (const version of tool.installed) {
    const [major, minor] = version.version.split(".");
    if (major !== undefined && minor !== undefined) series.add(`${major}.${minor}`);
  }
  return [...series].sort((a, b) => b.localeCompare(a, "en", { numeric: true }));
}
