// Pure model of the quick-settings form (docs/04 §1.3, docs/07 §5.3): the text form of each
// value (what `services.set` takes), validation per control kind, dirty tracking and the
// "reload or restart" outcome of a set of changes. No React, no tool names.
import type {
  QuickSettingDto,
  QuickSettingKind,
  ServicePortSettingDto,
  SettingApply,
  SettingValueDto,
} from "../../../ipc/generated";

/** Text form of a boolean. */
export const TRUE = "true";
export const FALSE = "false";

/** The eight control kinds of the manifest grammar, in the docs/04 order. */
export const QUICK_SETTING_KINDS: readonly QuickSettingKind[] = [
  "toggle",
  "select",
  "number",
  "text",
  "path",
  "port",
  "slider",
  "multi",
];

/** Draft of the form: `services.set` key to the text form of the value. */
export type Draft = Readonly<Record<string, string>>;

/** Key of a named port in `services.set` (`ports.main`). */
export function portKey(name: string): string {
  return `ports.${name}`;
}

/** Text form of a value, as `services.set` takes it (`true`, `1G`, `a,b`). */
export function valueText(value: SettingValueDto | null | undefined): string {
  if (value === null || value === undefined) return "";
  if (Array.isArray(value)) return value.join(",");
  return String(value);
}

/** Items of a `multi` text. */
export function multiItems(text: string): string[] {
  return text === "" ? [] : text.split(",");
}

/** Text of a named port: the reserved one, else the manifest's default. */
export function portText(port: ServicePortSettingDto): string {
  return port.port === null || port.port === undefined
    ? port.default === null || port.default === undefined
      ? ""
      : String(port.default)
    : String(port.port);
}

/** The saved state of the form. */
export function baselineOf(
  settings: readonly QuickSettingDto[],
  ports: readonly ServicePortSettingDto[],
): Draft {
  const draft: Record<string, string> = {};
  for (const setting of settings) draft[setting.key] = valueText(setting.value);
  for (const port of ports) draft[portKey(port.name)] = portText(port);
  return draft;
}

/** Why a value is not accepted; the component turns it into localized text. */
export type SettingProblem =
  | { code: "number" }
  | { code: "range"; min: number | null; max: number | null }
  | { code: "option" }
  | { code: "port" }
  | { code: "path" };

const NUMBER = /^-?\d+(\.\d+)?$/;
// eslint-disable-next-line no-control-regex -- control characters are exactly what is rejected
const CONTROL = /[\u0000-\u001f]/;

/** Validates the text of one setting; `undefined` when it can be sent. */
export function validateSetting(
  setting: QuickSettingDto,
  text: string,
): SettingProblem | undefined {
  const min = setting.min ?? null;
  const max = setting.max ?? null;
  switch (setting.kind) {
    case "toggle":
      return text === TRUE || text === FALSE ? undefined : { code: "option" };
    case "select":
      return (setting.options ?? []).length === 0 || (setting.options ?? []).includes(text)
        ? undefined
        : { code: "option" };
    case "number":
    case "slider": {
      if (!NUMBER.test(text)) return { code: "number" };
      const value = Number(text);
      if ((min !== null && value < min) || (max !== null && value > max)) {
        return { code: "range", min, max };
      }
      return undefined;
    }
    case "port": {
      if (!/^\d{1,5}$/.test(text)) return { code: "port" };
      const value = Number(text);
      return value >= 1 && value <= 65535 ? undefined : { code: "port" };
    }
    case "path":
      return CONTROL.test(text) ? { code: "path" } : undefined;
    case "multi": {
      const options = setting.options ?? [];
      return multiItems(text).every((item) => options.includes(item))
        ? undefined
        : { code: "option" };
    }
    case "text":
      return CONTROL.test(text) ? { code: "path" } : undefined;
  }
}

/** Keys whose draft differs from the saved state, in key order. */
export function dirtyKeys(baseline: Draft, draft: Draft): string[] {
  return Object.keys(draft)
    .filter((key) => draft[key] !== baseline[key])
    .sort();
}

/** The `services.set` map for the changed keys only. */
export function changedSettings(baseline: Draft, draft: Draft): Record<string, string> {
  return Object.fromEntries(dirtyKeys(baseline, draft).map((key) => [key, draft[key] ?? ""]));
}

/** What applying the changes takes: a port change always restarts; else the strongest `apply`. */
export function applyOf(
  settings: readonly QuickSettingDto[],
  changed: readonly string[],
): SettingApply | null {
  if (changed.length === 0) return null;
  let result: SettingApply = "none";
  for (const key of changed) {
    const apply: SettingApply = key.startsWith("ports.")
      ? "restart"
      : (settings.find((s) => s.key === key)?.apply ?? "none");
    if (apply === "restart") return "restart";
    if (apply === "reload") result = "reload";
  }
  return result;
}

/** Value a field goes back to: the saved one when changed, else the default (if different). */
export function resetTarget(
  setting: QuickSettingDto,
  baseline: Draft,
  draft: Draft,
): { kind: "undo" | "default"; text: string } | null {
  const saved = baseline[setting.key] ?? "";
  if ((draft[setting.key] ?? "") !== saved) return { kind: "undo", text: saved };
  const fallback = valueText(setting.default);
  if (setting.default !== null && setting.default !== undefined && fallback !== saved) {
    return { kind: "default", text: fallback };
  }
  return null;
}

/** A line of a config file and whether it sits inside a `# forgebox:begin/end` block. */
export interface ViewerLine {
  text: string;
  managed: boolean;
}

const BEGIN = /^\s*(#|;|\/\/)\s*forgebox:begin\b/;
const END = /^\s*(#|;|\/\/)\s*forgebox:end\b/;

/** Splits a file into lines, marking the managed blocks (markers included). */
export function markManagedLines(content: string): ViewerLine[] {
  let inside = false;
  return content.split(/\r?\n/).map((text) => {
    if (BEGIN.test(text)) inside = true;
    const managed = inside;
    if (END.test(text)) inside = false;
    return { text, managed };
  });
}
