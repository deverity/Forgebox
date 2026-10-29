// The `forge` command equivalent to an API call (ADR-0037, docs/07 §1.7): the TypeScript
// twin of `forge_ipc::api::cli_template`. It renders `METHODS[method].cli_template` from the
// same params the GUI sends to the daemon, so the screen never writes CLI text by hand.
// Both renderers must produce the strings of the shared golden file
// `crates/forge-ipc/tests/fixtures/cli_templates.json` (checked by `cli.test.ts`).
//
// First written by M2-E1-T9 (`describeError` actions need their commands); M2-E1-T13 adds
// `<CliCommand>` and the action registry on top of it.
//
// Grammar (full text in ADR-0037): alternatives separated by ` | `, each optionally led by
// a rule `[cond,cond]` (`field=value`, `field` truthy, `!field`); tokens are literal words or
// one placeholder: `{field}`, `{field=--flag}`, `{field?--flag}`, `{field?a:b}`,
// `{list...}`, with dotted paths (`{a.b}`). Anything that cannot be written portably for
// PowerShell and `cmd.exe` makes the command unavailable (`null`), never a wrong one.
import { METHODS, type MethodName } from "./methods";

type Json = unknown;

type Piece =
  | { kind: "literal"; word: string }
  | { kind: "positional"; path: readonly string[] }
  | { kind: "option"; path: readonly string[]; flag: string }
  | { kind: "flag"; path: readonly string[]; flag: string }
  | { kind: "choice"; path: readonly string[]; yes: string; no: string }
  | { kind: "variadic"; path: readonly string[] };

interface Condition {
  path: readonly string[];
  negate: boolean;
  equals: string | null;
}

interface Alternative {
  conditions: readonly Condition[];
  pieces: readonly Piece[];
}

/** A template that does not follow the grammar. */
export class CliTemplateError extends Error {
  override readonly name = "CliTemplateError";
}

const PATH_SEGMENT = /^[A-Za-z0-9_]+$/;

function parsePath(text: string): string[] {
  const segments = text.split(".");
  if (segments.every((segment) => PATH_SEGMENT.test(segment))) return segments;
  throw new CliTemplateError(`bad field path ${JSON.stringify(text)}`);
}

function isWord(text: string): boolean {
  return text.length > 0 && !/[{}[\]]/.test(text);
}

function parseCondition(text: string): Condition {
  const negate = text.startsWith("!");
  const rest = negate ? text.slice(1) : text;
  const eq = rest.indexOf("=");
  if (eq >= 0) {
    const value = rest.slice(eq + 1);
    if (negate || !isWord(value)) throw new CliTemplateError(`bad condition ${text}`);
    return { path: parsePath(rest.slice(0, eq)), negate, equals: value };
  }
  return { path: parsePath(rest), negate, equals: null };
}

function parsePlaceholder(inner: string): Piece {
  let pathText: string;
  let rest: string;
  if (inner.endsWith("...")) {
    pathText = inner.slice(0, -3);
    rest = "...";
  } else {
    const match = /[^A-Za-z0-9_.]/.exec(inner);
    const end = match === null ? inner.length : match.index;
    pathText = inner.slice(0, end);
    rest = inner.slice(end);
  }
  const path = parsePath(pathText);
  if (rest === "") return { kind: "positional", path };
  if (rest === "...") return { kind: "variadic", path };
  if (rest.startsWith("=")) {
    const flag = rest.slice(1);
    if (isWord(flag) && flag.startsWith("-")) return { kind: "option", path, flag };
    throw new CliTemplateError(`bad option flag in {${inner}}`);
  }
  if (rest.startsWith("?")) {
    const choice = rest.slice(1);
    const colon = choice.indexOf(":");
    if (colon < 0) {
      if (isWord(choice) && choice.startsWith("-")) return { kind: "flag", path, flag: choice };
    } else {
      const yes = choice.slice(0, colon);
      const no = choice.slice(colon + 1);
      if ((yes === "" || isWord(yes)) && (no === "" || isWord(no)) && !(yes === "" && no === "")) {
        return { kind: "choice", path, yes, no };
      }
    }
    throw new CliTemplateError(`bad flag or choice in {${inner}}`);
  }
  throw new CliTemplateError(`bad placeholder {${inner}}`);
}

function parseAlternative(text: string): Alternative {
  const tokens = text.split(/\s+/).filter((token) => token !== "");
  const conditions: Condition[] = [];
  const first = tokens[0];
  if (first?.startsWith("[")) {
    if (!first.endsWith("]")) throw new CliTemplateError(`unterminated rule in ${text}`);
    for (const condition of first.slice(1, -1).split(",")) {
      conditions.push(parseCondition(condition));
    }
    tokens.shift();
  }
  const pieces: Piece[] = tokens.map((token) => {
    if (token.startsWith("{")) {
      if (!token.endsWith("}")) throw new CliTemplateError(`unterminated placeholder ${token}`);
      return parsePlaceholder(token.slice(1, -1));
    }
    if (isWord(token)) return { kind: "literal", word: token };
    throw new CliTemplateError(`bad token ${token}`);
  });
  if (pieces[0]?.kind !== "literal") {
    throw new CliTemplateError(`${text} must start with a literal word`);
  }
  return { conditions, pieces };
}

/** Parses a template (throws `CliTemplateError`). */
export function parseCliTemplate(source: string): readonly Alternative[] {
  return source.split(" | ").map(parseAlternative);
}

function isRecord(value: Json): value is Record<string, Json> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function get(params: Json, path: readonly string[]): Json {
  let current: Json = params;
  for (const segment of path) {
    if (!isRecord(current) || !Object.hasOwn(current, segment)) return undefined;
    current = current[segment];
  }
  return current;
}

function truthy(value: Json): boolean {
  if (value === undefined || value === null || value === false) return false;
  if (typeof value === "string") return value !== "";
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === "object") return Object.keys(value).length > 0;
  return true;
}

const UNREPRESENTABLE = Symbol("unrepresentable");
type Scalar = string | null | typeof UNREPRESENTABLE;

/** Text of a scalar: `null` when absent (`null`, `""`), `UNREPRESENTABLE` for the rest. */
function scalarText(value: Json): Scalar {
  if (value === null || value === undefined) return null;
  if (typeof value === "string") return value === "" ? null : value;
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number" && Number.isSafeInteger(value)) return String(value);
  return UNREPRESENTABLE;
}

function holds(condition: Condition, params: Json): boolean {
  const value = get(params, condition.path);
  let result: boolean;
  if (condition.equals === null) {
    result = value !== undefined && truthy(value);
  } else {
    const text = value === undefined ? null : scalarText(value);
    result = typeof text === "string" && text === condition.equals;
  }
  return result !== condition.negate;
}

function isBareChar(c: string): boolean {
  return /^[A-Za-z0-9_+=:./\\@-]$/.test(c);
}

function isUnquotable(c: string): boolean {
  const code = c.codePointAt(0) ?? 0;
  return (
    c === '"' ||
    c === "$" ||
    c === "`" ||
    c === "%" ||
    code <= 0x1f ||
    (code >= 0x7f && code <= 0x9f)
  );
}

/**
 * Quotes one argument for PowerShell and `cmd.exe` at once (same rules as the Rust
 * `quote_arg`): bare when made of `[A-Za-z0-9_+=:./\@-]` (not starting with `@`), else in
 * double quotes with a trailing run of `\` doubled; `null` for what no common quoting
 * survives (empty, `"`, `$`, `` ` ``, `%`, control characters).
 */
export function quoteArg(value: string): string | null {
  const chars = Array.from(value);
  if (chars.length === 0 || chars.some(isUnquotable)) return null;
  if (!value.startsWith("@") && chars.every(isBareChar)) return value;
  const trailing = value.length - value.replace(/\\+$/, "").length;
  return `"${value}${"\\".repeat(trailing)}"`;
}

/** A value as one argument; values starting with `-` would read as flags. */
function argument(text: string): string | typeof UNREPRESENTABLE {
  if (text.startsWith("-")) return UNREPRESENTABLE;
  return quoteArg(text) ?? UNREPRESENTABLE;
}

/** Renders one piece into `out`; false when the command cannot be written. */
function renderPiece(piece: Piece, params: Json, out: string[]): boolean {
  switch (piece.kind) {
    case "literal":
      out.push(piece.word);
      return true;
    case "positional":
    case "option": {
      const value = get(params, piece.path);
      const text = value === undefined ? null : scalarText(value);
      if (text === UNREPRESENTABLE) return false;
      if (text === null) return true;
      const arg = argument(text);
      if (arg === UNREPRESENTABLE) return false;
      if (piece.kind === "option") out.push(piece.flag);
      out.push(arg);
      return true;
    }
    case "flag":
      if (get(params, piece.path) === true) out.push(piece.flag);
      return true;
    case "choice": {
      const value = get(params, piece.path);
      const word = value === true ? piece.yes : value === false ? piece.no : "";
      if (word !== "") out.push(word);
      return true;
    }
    case "variadic": {
      const value = get(params, piece.path);
      if (value === undefined || value === null) return true;
      if (Array.isArray(value)) {
        for (const item of value as Json[]) {
          const text = scalarText(item);
          if (text === UNREPRESENTABLE) return false;
          if (text === null) continue;
          const arg = argument(text);
          if (arg === UNREPRESENTABLE) return false;
          out.push(arg);
        }
        return true;
      }
      if (isRecord(value)) {
        // Rust sorts the map keys by bytes; plain `<` on UTF-16 matches it for the BMP.
        const keys = Object.keys(value).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
        for (const key of keys) {
          const text = scalarText(value[key]);
          if (text === UNREPRESENTABLE) return false;
          const arg = argument(`${key}=${text ?? ""}`);
          if (arg === UNREPRESENTABLE) return false;
          out.push(arg);
        }
        return true;
      }
      return false;
    }
  }
}

/** Renders `template` for `params`; `null` without a matching alternative or a portable form. */
export function renderTemplate(template: string, params: Json): string | null {
  let alternatives: readonly Alternative[];
  try {
    alternatives = parseCliTemplate(template);
  } catch {
    return null;
  }
  const chosen = alternatives.find((alt) => alt.conditions.every((c) => holds(c, params)));
  if (chosen === undefined) return null;
  const out: string[] = [];
  for (const piece of chosen.pieces) {
    if (!renderPiece(piece, params, out)) return null;
  }
  return out.join(" ");
}

/** The `forge` command equivalent to calling `method` with `params` (`null` when there is none). */
export function renderCli(method: MethodName, params: Json = {}): string | null {
  const template = METHODS[method].cli_template as string | null;
  return template === null ? null : renderTemplate(template, params);
}
