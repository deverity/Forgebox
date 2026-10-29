import { useTranslation } from "react-i18next";
import { Select } from "../../design/primitives/Select";
import type { LoadedSelectProps } from "./ContextSelector";

/** The Radix select of the context selector: a lazy chunk (Radix stays out of the first paint). */
export default function ContextSelect({ active, options, nameOf, onSelect }: LoadedSelectProps) {
  const { t } = useTranslation("shell");
  return (
    <Select
      label={t("context.label")}
      width="full"
      value={active}
      options={options.map((o) => ({ value: o.id, label: nameOf(o), disabled: o.disabled }))}
      onValueChange={onSelect}
    />
  );
}
