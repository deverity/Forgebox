import { useId } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { Button } from "../../../design/primitives/Button";
import { PathField } from "../../../design/primitives/PathField";
import { PortField } from "../../../design/primitives/PortField";
import { Select } from "../../../design/primitives/Select";
import { Slider } from "../../../design/primitives/Slider";
import { TextField } from "../../../design/primitives/TextField";
import { Toggle } from "../../../design/primitives/Toggle";
import { TNUM_CLASS } from "../../../i18n/format";
import type { PortCheckDto, QuickSettingDto } from "../../../ipc/generated";
import { FALSE, TRUE, multiItems, type SettingProblem } from "./model";

const BULLET = "·";

export interface QuickSettingFieldProps {
  setting: QuickSettingDto;
  /** Text form of the current draft value. */
  value: string;
  onChange: (value: string) => void;
  /** Localized problem of the draft value (only shown for changed fields). */
  error: string | undefined;
  /** Reset control: what it does and its click handler; `null` hides it. */
  reset: { kind: "undo" | "default"; onClick: () => void } | null;
  /** The port is held by another owner (marks the field). */
  conflict?: boolean;
  /** Port kind: latest `net.port_check` answer. */
  onPortResult?: (result: PortCheckDto | null) => void;
}

/** Localized text of a validation problem. */
export function problemText(t: TFunction<"services">, problem: SettingProblem): string {
  switch (problem.code) {
    case "number":
      return t("quick.problem.number");
    case "option":
      return t("quick.problem.option");
    case "port":
      return t("quick.problem.port");
    case "path":
      return t("quick.problem.path");
    case "range":
      if (problem.min !== null && problem.max !== null) {
        return t("quick.problem.rangeBoth", { min: problem.min, max: problem.max });
      }
      return problem.min === null
        ? t("quick.problem.rangeMax", { max: problem.max ?? 0 })
        : t("quick.problem.rangeMin", { min: problem.min });
  }
}

function Control({ setting, value, onChange, error, onPortResult }: QuickSettingFieldProps) {
  const label = setting.label ?? setting.key;
  const errorProp = error === undefined ? {} : { error };
  const options = (setting.options ?? []).map((option) => ({ value: option, label: option }));
  const groupId = useId();
  switch (setting.kind) {
    case "toggle":
      return (
        <div className="flex items-center gap-2">
          <Toggle
            label={label}
            checked={value === TRUE}
            onCheckedChange={(next) => {
              onChange(next ? TRUE : FALSE);
            }}
          />
          <span className="text-callout text-fg-secondary" aria-hidden="true">
            {label}
          </span>
        </div>
      );
    case "select":
      return (
        <div className="flex flex-col gap-1">
          <span className="text-callout text-fg-secondary" aria-hidden="true">
            {label}
          </span>
          <Select label={label} options={options} value={value} onValueChange={onChange} />
        </div>
      );
    case "slider":
      return (
        <div className="flex flex-col gap-1">
          <span className="text-callout text-fg-secondary" aria-hidden="true">
            {label} {BULLET} <span className={TNUM_CLASS}>{value}</span>
          </span>
          <Slider
            label={label}
            value={Number.isFinite(Number(value)) ? Number(value) : (setting.min ?? 0)}
            min={setting.min ?? 0}
            max={setting.max ?? 100}
            step={setting.step ?? 1}
            valueText={value}
            onValueChange={(next) => {
              onChange(String(next));
            }}
          />
        </div>
      );
    case "number":
      return (
        <TextField
          label={label}
          value={value}
          inputMode="decimal"
          autoComplete="off"
          {...errorProp}
          onChange={(event) => {
            onChange(event.target.value);
          }}
        />
      );
    case "text":
      return (
        <TextField
          label={label}
          value={value}
          autoComplete="off"
          {...errorProp}
          onChange={(event) => {
            onChange(event.target.value);
          }}
        />
      );
    case "path":
      return (
        <PathField
          label={label}
          value={value}
          kind="file"
          showDisabledBrowse
          {...errorProp}
          onChange={onChange}
        />
      );
    case "port":
      return (
        <PortField
          label={label}
          value={value}
          {...errorProp}
          {...(onPortResult === undefined ? {} : { onResult: onPortResult })}
          onChange={onChange}
        />
      );
    case "multi": {
      const chosen = multiItems(value);
      return (
        <fieldset aria-labelledby={groupId} className="flex flex-col gap-1">
          <legend id={groupId} className="text-callout text-fg-secondary">
            {label}
          </legend>
          {(setting.options ?? []).map((option) => (
            <label key={option} className="flex items-center gap-2 text-callout">
              <input
                type="checkbox"
                className="accent-accent"
                checked={chosen.includes(option)}
                onChange={(event) => {
                  const next = event.target.checked
                    ? [...chosen, option]
                    : chosen.filter((item) => item !== option);
                  // Keep the manifest's option order so the text form is canonical.
                  onChange((setting.options ?? []).filter((item) => next.includes(item)).join(","));
                }}
              />
              {option}
            </label>
          ))}
          {error === undefined ? null : (
            <p role="alert" className="text-caption text-state-failed">
              {error}
            </p>
          )}
        </fieldset>
      );
    }
  }
}

/** One quick setting: its control, the apply hint and the per-field reset. */
export function QuickSettingField(props: QuickSettingFieldProps) {
  const { t } = useTranslation("services");
  const { setting, reset } = props;
  return (
    <div
      data-setting={setting.key}
      data-kind={setting.kind}
      data-dirty={reset?.kind === "undo" ? "true" : undefined}
      data-conflict={props.conflict === true ? "true" : undefined}
      className="flex items-end justify-between gap-2"
    >
      <Control {...props} />
      {reset === null ? null : (
        <Button variant="ghost" onClick={reset.onClick}>
          <span className="sr-only">{setting.label ?? setting.key}: </span>
          {reset.kind === "undo" ? t("quick.undo") : t("quick.useDefault")}
        </Button>
      )}
    </div>
  );
}
