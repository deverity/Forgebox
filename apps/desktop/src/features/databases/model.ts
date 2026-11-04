// Pure helpers of the Databases screen (M2-E3-T14): name validation with the daemon's regex,
// charsets per protocol, the site-to-database name, the connection strings and their masks,
// the import path rules and the last lines of a client's stderr. No React, no IPC calls.
import type * as T from "../../ipc/generated";

/** `forge_manifest::database`: names are `^[A-Za-z0-9_]{1,64}$` and case-sensitive. */
export const DB_NAME_RE = /^[A-Za-z0-9_]{1,64}$/;

export type NameProblem = "required" | "invalid" | "exists";

/** The first reason `name` cannot be created (`undefined`: valid). `existing` compares exactly. */
export function nameProblem(name: string, existing: readonly string[]): NameProblem | undefined {
  if (name === "") return "required";
  if (!DB_NAME_RE.test(name)) return "invalid";
  return existing.includes(name) ? "exists" : undefined;
}

const MYSQL_CHARSETS = ["utf8mb4", "utf8mb3", "latin1"] as const;
const POSTGRES_CHARSETS = ["UTF8", "LATIN1", "SQL_ASCII"] as const;

/**
 * Charsets offered by the create form; the first is the engine's default (`utf8mb4`, `UTF8`).
 * The daemon validates the charset again against `[A-Za-z0-9_]{1,32}`.
 */
export function charsetOptions(protocol: string): readonly string[] {
  return protocol === "postgresql" ? POSTGRES_CHARSETS : MYSQL_CHARSETS;
}

/** `loja-x.test` to `loja_x`: the database name suggested for a site host. */
export function suggestDatabaseName(host: string): string {
  const label = host.toLowerCase().split(".")[0] ?? host;
  const cleaned = label.replace(/[^a-z0-9_]+/g, "_").replace(/^_+|_+$/g, "");
  return cleaned.slice(0, 64);
}

const PROTOCOL_MONOGRAM: Record<string, string> = { mysql: "My", postgresql: "Pg" };

/** Two letters on the instance card in place of an icon (the tool icon loads lazily elsewhere). */
export function monogram(instance: T.DbInstanceDto): string {
  return PROTOCOL_MONOGRAM[instance.protocol] ?? instance.tool.slice(0, 2);
}

/** The instance the page shows first: the chosen one, else a running one, else the first. */
export function pickInstance(
  instances: readonly T.DbInstanceDto[],
  chosen: string | null,
): T.DbInstanceDto | undefined {
  return (
    instances.find((i) => i.instance === chosen) ??
    instances.find((i) => i.state === "running") ??
    instances[0]
  );
}

/** Databases of one instance, by name. */
export function databasesOf(
  list: readonly T.DatabaseDto[],
  instance: string,
): readonly T.DatabaseDto[] {
  return list
    .filter((db) => db.instance === instance)
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

/** Charset and collation in one cell (`utf8mb4 / utf8mb4_0900_ai_ci`); `null` when unknown. */
export function charsetText(db: T.DatabaseDto): string | null {
  if (db.charset === null) return null;
  return db.collation === null ? db.charset : `${db.charset} / ${db.collation}`;
}

// --- connection strings ---------------------------------------------------------------------

export const SECRET_MASK = "••••••••";
const SECRET_KEY_RE = /PASS|PWD|SECRET|TOKEN/i;
const URL_CREDENTIALS_RE = /(:\/\/[^:@/\s]*:)([^@\s]*)(@)/;

/** Replaces the password of `scheme://user:pass@host` (an empty one has nothing to hide). */
export function maskUrl(value: string): string {
  return value.replace(URL_CREDENTIALS_RE, (_all, head: string, secret: string, at: string) =>
    secret === "" ? `${head}${at}` : `${head}${SECRET_MASK}${at}`,
  );
}

/** The `.env` snippet (`KEY=value` per line, sorted by key). */
export function envSnippet(vars: Readonly<Record<string, string>>, masked: boolean): string {
  return Object.keys(vars)
    .sort()
    .map((key) => {
      const value = vars[key] ?? "";
      if (!masked) return `${key}=${value}`;
      return `${key}=${SECRET_KEY_RE.test(key) && value !== "" ? SECRET_MASK : maskUrl(value)}`;
    })
    .join("\n");
}

/** PDO-style DSN built from the `.env` variables; `null` when they do not carry a host. */
export function dsn(protocol: string, vars: Readonly<Record<string, string>>): string | null {
  const host = vars.DB_HOST;
  const name = vars.DB_DATABASE;
  if (host === undefined || name === undefined) return null;
  const driver = protocol === "postgresql" ? "pgsql" : "mysql";
  const port = vars.DB_PORT === undefined || vars.DB_PORT === "" ? "" : `;port=${vars.DB_PORT}`;
  return `${driver}:host=${host}${port};dbname=${name}`;
}

export interface ShownStrings {
  dsn: string | null;
  url: string;
  env: string;
}

/** The three strings as shown: the credentials are replaced until the user reveals them. */
export function shownStrings(
  protocol: string,
  result: T.DbUrlDto,
  revealed: boolean,
): ShownStrings {
  const vars = result.env.vars;
  return {
    dsn: dsn(protocol, vars),
    url: revealed ? result.url : maskUrl(result.url),
    env: envSnippet(vars, !revealed),
  };
}

// --- import paths ---------------------------------------------------------------------------

const DUMP_EXTENSIONS = [".sql", ".sql.gz", ".sql.zst"] as const;

/** Whether the file name is one of the three formats the daemon reads. */
export function hasDumpExtension(path: string): boolean {
  const lower = path.toLowerCase();
  return DUMP_EXTENSIONS.some((ext) => lower.endsWith(ext));
}

/** A remote daemon only reads inside its `www/` folder (docs/11 §5): a quick client-side check. */
export function insideWww(path: string): boolean {
  return path.replaceAll("\\", "/").toLowerCase().split("/").includes("www");
}

export type ImportProblem = "required" | "badExtension" | "remoteOutside";

export function importProblem(path: string, remote: boolean): ImportProblem | undefined {
  if (path.trim() === "") return "required";
  if (!hasDumpExtension(path)) return "badExtension";
  if (remote && !insideWww(path)) return "remoteOutside";
  return undefined;
}

// --- results and failures -------------------------------------------------------------------

export const STDERR_LINES = 20;

/**
 * The client output a failed transfer carries: the daemon puts the last 20 stderr lines after the
 * first line of the job error's message (`transfer.rs`). The first line stays out.
 */
export function stderrTail(message: string): string {
  const lines = message.split("\n").map((line) => line.trimEnd());
  return lines
    .slice(1)
    .filter((line) => line !== "")
    .slice(-STDERR_LINES)
    .join("\n");
}

/** The `DbExportDto` of a finished export job (`null` when the result has another shape). */
export function exportResult(result: unknown): { path: string; size: number } | null {
  if (typeof result !== "object" || result === null) return null;
  const { path, size_bytes: size } = result as { path?: unknown; size_bytes?: unknown };
  return typeof path === "string" ? { path, size: typeof size === "number" ? size : 0 } : null;
}
