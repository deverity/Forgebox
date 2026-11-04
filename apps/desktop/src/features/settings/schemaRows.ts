import type { TFunction } from "i18next";
import type { ConfigDocumentDto } from "../../ipc/generated";
import { loose } from "./looseT";
import { EDITOR_KEYS, buildRows, withKnownOptions, type RowSpec } from "./model";

/** Rows of the Editor/Terminal section found in the schema, in display order. */
export function editorRows(doc: ConfigDocumentDto | undefined): RowSpec[] {
  if (doc === undefined) return [];
  const all = buildRows(doc);
  return EDITOR_KEYS.flatMap((key) => {
    const row = all.find((candidate) => candidate.key === key);
    return row === undefined ? [] : [withKnownOptions(row)];
  });
}

const slug = (key: string) => key.replace(/\./g, "_");

/** Localized label of a key; falls back to the schema title for keys without a translation. */
export function rowLabel(t: TFunction<"settings">, row: RowSpec): string {
  return loose(t)(`rows.${slug(row.key)}.label`, { defaultValue: row.title });
}

export function rowDescription(t: TFunction<"settings">, row: RowSpec): string {
  // The text names placeholders in braces; i18next interpolates with "{", so pass them back.
  return loose(t)(`rows.${slug(row.key)}.description`, {
    defaultValue: row.description,
    path: "{path}",
    line: "{line}",
    column: "{column}",
    dir: "{dir}",
  });
}
