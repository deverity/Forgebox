// Settings model (docs/07 §5.11): turns the JSON Schema of `system.config_list` into row specs,
// reads values by dotted key and encodes what the user typed back into the JSON the daemon
// takes. Pure TypeScript; the daemon stays the validator (`config_invalid`), this only picks
// the control. A schema node it does not understand yields no row (never a wrong control).
import type { ConfigDocumentDto } from "../../ipc/generated";

export type ControlKind = "toggle" | "number" | "select" | "text" | "list";

export interface RowSpec {
  /** Dotted key of `forgebox.toml` (`dns.enabled`, `editor.command`). */
  key: string;
  control: ControlKind;
  /** Schema title, or the humanized last segment of the key. */
  title: string;
  description: string;
  /** Values of a `select`; the empty string stands for "unset" on a nullable key. */
  options: readonly string[];
  min?: number;
  max?: number;
  integer: boolean;
  /** `null` is a valid value (unset: the daemon decides). */
  nullable: boolean;
}

type Json = unknown;
type Node = Record<string, Json>;

function isNode(value: Json): value is Node {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function without(node: Node, key: string): Node {
  return Object.fromEntries(Object.entries(node).filter(([name]) => name !== key));
}

/** Follows `#/$defs/X` (the only reference form schemars writes). */
function deref(root: Node, node: Node): Node {
  const ref = node.$ref;
  if (typeof ref !== "string" || !ref.startsWith("#/")) return node;
  let target: Json = root;
  for (const part of ref.slice(2).split("/")) {
    target = isNode(target) ? target[part] : undefined;
  }
  if (!isNode(target)) return node;
  // Siblings of the `$ref` (description of the field) win over the definition's.
  return { ...target, ...without(node, "$ref") };
}

interface Resolved {
  node: Node;
  nullable: boolean;
}

/** Unwraps `anyOf/oneOf/allOf` with `null` and `type: [X, "null"]` into one node + nullable. */
function resolve(root: Node, input: Node): Resolved {
  let node = deref(root, input);
  let nullable = false;
  const type = node.type;
  if (Array.isArray(type)) {
    const rest = type.filter((t) => t !== "null");
    nullable = rest.length !== type.length;
    node = { ...node, type: rest.length === 1 ? rest[0] : rest };
  }
  for (const word of ["anyOf", "oneOf", "allOf"] as const) {
    const branches = node[word];
    if (!Array.isArray(branches) || !branches.every(isNode)) continue;
    const real = branches.filter((b) => b.type !== "null");
    if (real.length !== branches.length) nullable = true;
    const only = real[0];
    if (real.length === 1 && only !== undefined) {
      const inner = resolve(root, only);
      const outer = without(node, word);
      return { node: { ...inner.node, ...outer }, nullable: nullable || inner.nullable };
    }
  }
  return { node, nullable };
}

/** `enum` and `oneOf` of string `const`s. */
function optionsOf(node: Node): string[] | null {
  const strings = (list: readonly Json[]): string[] | null =>
    list.every((v): v is string => typeof v === "string") ? list.slice() : null;
  if (Array.isArray(node.enum)) return strings(node.enum as Json[]);
  const branches = node.oneOf;
  if (Array.isArray(branches) && branches.length > 0 && branches.every(isNode)) {
    return strings(
      branches.map(
        (b): Json => b.const ?? (Array.isArray(b.enum) ? (b.enum as Json[])[0] : undefined),
      ),
    );
  }
  return null;
}

/** `ports.reserved` -> `Reserved`, `quiet_hours` -> `Quiet hours`. */
export function humanizeKey(key: string): string {
  const last = key.split(".").pop() ?? key;
  const words = last.replace(/_/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

function text(value: Json): string {
  return typeof value === "string" ? value.trim() : "";
}

/** Row for one property, or `null` when its type has no control. */
function rowOf(root: Node, key: string, schema: Node): RowSpec | null {
  const { node, nullable } = resolve(root, schema);
  const base = {
    key,
    title: text(node.title) || humanizeKey(key),
    description: text(node.description),
    nullable,
    integer: false,
  };
  const options = optionsOf(node);
  if (options !== null) {
    return { ...base, control: "select", options: nullable ? ["", ...options] : options };
  }
  switch (node.type) {
    case "boolean":
      return { ...base, control: "toggle", options: [] };
    case "integer":
    case "number": {
      const row: RowSpec = {
        ...base,
        control: "number",
        options: [],
        integer: node.type === "integer",
      };
      if (typeof node.minimum === "number") row.min = node.minimum;
      if (typeof node.maximum === "number") row.max = node.maximum;
      return row;
    }
    case "string":
      return { ...base, control: "text", options: [] };
    case "array": {
      const items = isNode(node.items) ? resolve(root, node.items).node : null;
      return items?.type === "string" ? { ...base, control: "list", options: [] } : null;
    }
    default:
      return null;
  }
}

/** Walks object properties depth-first; a node with no control and no properties is skipped. */
function walk(root: Node, prefix: string, schema: Node, out: RowSpec[]): void {
  const { node } = resolve(root, schema);
  const props = node.properties;
  if (node.type === "object" && isNode(props)) {
    for (const [name, child] of Object.entries(props)) {
      if (isNode(child)) walk(root, prefix === "" ? name : `${prefix}.${name}`, child, out);
    }
    return;
  }
  if (prefix === "") return;
  const row = rowOf(root, prefix, schema);
  if (row !== null) out.push(row);
}

/** Every configurable scalar of the schema, in schema order. */
export function buildRows(doc: Pick<ConfigDocumentDto, "schema">): RowSpec[] {
  const out: RowSpec[] = [];
  if (isNode(doc.schema)) walk(doc.schema, "", doc.schema, out);
  return out;
}

/** `values.dns.enabled` for `dns.enabled`; `undefined` when absent. */
export function valueAt(values: Json, key: string): Json {
  let current: Json = values;
  for (const part of key.split(".")) {
    if (!isNode(current)) return undefined;
    current = current[part];
  }
  return current;
}

/** What a control shows for a value (`null`/absent: empty). */
export function toDraft(row: RowSpec, value: Json): string {
  if (value === null || value === undefined) return "";
  if (row.control === "list" && Array.isArray(value)) return value.join(", ");
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}

export type Encoded = { ok: true; value: Json } | { ok: false; problem: "number" | "range" };

/** The JSON to send for the text a control holds. Empty on a nullable key unsets it. */
export function encode(row: RowSpec, draft: string): Encoded {
  const trimmed = draft.trim();
  switch (row.control) {
    case "toggle":
      return { ok: true, value: draft === "true" };
    case "number": {
      if (trimmed === "") {
        return row.nullable ? { ok: true, value: null } : { ok: false, problem: "number" };
      }
      const n = Number(trimmed);
      if (!Number.isFinite(n) || (row.integer && !Number.isInteger(n))) {
        return { ok: false, problem: "number" };
      }
      if ((row.min !== undefined && n < row.min) || (row.max !== undefined && n > row.max)) {
        return { ok: false, problem: "range" };
      }
      return { ok: true, value: n };
    }
    case "list":
      return {
        ok: true,
        value:
          trimmed === ""
            ? []
            : trimmed
                .split(",")
                .map((part) => part.trim())
                .filter((part) => part !== ""),
      };
    default:
      return { ok: true, value: trimmed === "" && row.nullable ? null : draft };
  }
}

/** Same JSON value (the values here are scalars and lists). */
export function sameValue(a: Json, b: Json): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}

// --- sections ---------------------------------------------------------------------------------

/** Keys of the Editor/Terminal section, in display order. */
export const EDITOR_KEYS = [
  "editor.command",
  "terminal.command",
  "terminal.integrated_shell",
  "terminal.max_sessions",
] as const;

/** Shells the integrated terminal knows (`INTEGRATED_SHELLS` in forge-core); not in the schema. */
export const INTEGRATED_SHELLS = ["pwsh", "powershell", "cmd"] as const;

/** Control overrides for keys the schema types more loosely than the daemon accepts. */
export function withKnownOptions(row: RowSpec): RowSpec {
  if (row.key === "terminal.integrated_shell") {
    return { ...row, control: "select", options: ["", ...INTEGRATED_SHELLS] };
  }
  return row;
}

export interface Preset {
  id: string;
  /** Command line stored in `forgebox.toml`; empty: unset (auto-detect). */
  command: string;
}

/** Editor presets (`{path}`, `{line}`, `{column}`); empty = auto-detect. */
export const EDITOR_PRESETS: readonly Preset[] = [
  { id: "auto", command: "" },
  { id: "vscode", command: "code -g {path}:{line}" },
  { id: "phpstorm", command: "phpstorm --line {line} {path}" },
  { id: "notepad", command: "notepad {path}" },
];

/** Terminal presets (`{dir}`); empty = auto-detect. */
export const TERMINAL_PRESETS: readonly Preset[] = [
  { id: "auto", command: "" },
  { id: "wt", command: "wt -d {dir}" },
  { id: "pwsh", command: "pwsh -NoExit -WorkingDirectory {dir}" },
  { id: "cmd", command: "cmd /k cd /d {dir}" },
];

export const PRESETS_BY_KEY: Readonly<Record<string, readonly Preset[]>> = {
  "editor.command": EDITOR_PRESETS,
  "terminal.command": TERMINAL_PRESETS,
};

/** Lowercase without accents, for the label search. */
export function normalizeSearch(value: string): string {
  return value.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase().trim();
}

export function matchesSearch(query: string, ...labels: readonly string[]): boolean {
  const q = normalizeSearch(query);
  return q === "" || labels.some((label) => normalizeSearch(label).includes(q));
}
