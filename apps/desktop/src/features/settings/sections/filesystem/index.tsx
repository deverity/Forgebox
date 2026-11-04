import { useTranslation } from "react-i18next";
import { loose, useRoot } from "../../looseT";
import { buildRows, matchesSearch, valueAt, withKnownOptions } from "../../model";
import type { SectionProps } from "../../registry";
import { SettingRow } from "../../SettingRow";
import { CaseFinder } from "./CaseFinder";
import { CaseTable } from "./CaseTable";
import { Converter } from "./Converter";

/** Row ids of the section (`filesystem.<id>.label` in the `settings` namespace). */
export const FILESYSTEM_ROWS = ["global", "table", "convert", "finder"] as const;

const KEY_CASE = "case_sensitive";

/**
 * Sistema de arquivos (docs/15): the global default, the status of each folder, the converter
 * job and the Case Mismatch Finder.
 */
export function FilesystemSection({ query, doc }: SectionProps) {
  const { t } = useTranslation("settings");
  const root = useRoot();
  const show = (id: (typeof FILESYSTEM_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`filesystem.${id}.label`));
  const spec = doc === undefined ? undefined : buildRows(doc).find((row) => row.key === KEY_CASE);
  return (
    <div className="flex flex-col">
      {show("global") && spec !== undefined && doc !== undefined ? (
        <SettingRow
          spec={withKnownOptions(spec)}
          label={t("filesystem.global.label")}
          description={t("filesystem.global.description")}
          value={valueAt(doc.values, KEY_CASE)}
        />
      ) : null}
      {show("table") ? <CaseTable root={root} /> : null}
      {show("convert") ? <Converter root={root} /> : null}
      {show("finder") ? <CaseFinder /> : null}
    </div>
  );
}
