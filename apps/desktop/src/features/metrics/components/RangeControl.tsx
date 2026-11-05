import { useTranslation } from "react-i18next";
import { SegmentedControl } from "../../../design/primitives/SegmentedControl";
import { RANGES, type PerformanceRange } from "./model";

/** 60 s (live stream) · 1 h · 24 h · 7 d. Each change but 60 s reads `stats.series`. */
export function RangeControl({
  value,
  onChange,
}: {
  value: PerformanceRange;
  onChange: (range: PerformanceRange) => void;
}) {
  const { t } = useTranslation("metrics");
  return (
    <SegmentedControl
      label={t("site.range.label")}
      value={value}
      options={RANGES.map((range) => ({ value: range, label: t(`site.range.${range}`) }))}
      onValueChange={(next) => {
        const range = RANGES.find((candidate) => candidate === next);
        if (range !== undefined) onChange(range);
      }}
    />
  );
}
