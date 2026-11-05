import { useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { updateInstance } from "../../../actions/services";
import { ActionButton } from "../../../design/primitives/ActionButton";
import { Select } from "../../../design/primitives/Select";
import { TextField } from "../../../design/primitives/TextField";
import { Toggle } from "../../../design/primitives/Toggle";
import {
  DISABLED,
  FIELD_BASE,
  FIELD_BORDER,
  FOCUS_RING,
  cx,
} from "../../../design/primitives/shared";
import type { InstanceAdvancedDto } from "../../../ipc/generated";
import { useForgeQuery } from "../../../state";
import { QueryBoundary } from "./QueryBoundary";
import {
  RESTART_ON,
  advancedPatch,
  advancedProblems,
  draftOf,
  type AdvancedDraft,
} from "./advancedModel";

export interface AdvancedTabProps {
  /** `mysql/default`. */
  target: string;
  /** Version the instance runs now. */
  version: string;
}

function TextArea({
  label,
  value,
  onChange,
  error,
  hint,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  error?: string | undefined;
  hint?: string;
}) {
  const id = useId();
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-callout text-fg-secondary">
        {label}
      </label>
      <textarea
        id={id}
        rows={4}
        spellCheck={false}
        value={value}
        aria-invalid={error === undefined ? undefined : true}
        aria-describedby={error === undefined ? undefined : `${id}-error`}
        onChange={(event) => {
          onChange(event.target.value);
        }}
        className={cx(
          FIELD_BASE,
          "h-auto w-full py-2 font-mono text-mono",
          error === undefined ? FIELD_BORDER : "border-state-failed",
          FOCUS_RING,
          DISABLED,
        )}
      />
      {error === undefined ? null : (
        <p id={`${id}-error`} role="alert" className="text-caption text-state-failed">
          {error}
        </p>
      )}
      {hint === undefined ? null : <p className="text-caption text-fg-tertiary">{hint}</p>}
    </div>
  );
}

function AdvancedForm({
  target,
  version,
  advanced,
}: {
  target: string;
  version: string;
  advanced: InstanceAdvancedDto;
}) {
  const { t } = useTranslation("services");
  const base = useMemo(() => draftOf(advanced, version), [advanced, version]);
  const [draft, setDraft] = useState<AdvancedDraft>(base);
  const patch = advancedPatch(base, draft);
  const problems = advancedProblems(draft);
  const set = <K extends keyof AdvancedDraft>(key: K, value: AdvancedDraft[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };
  const versions = [...new Set([version, ...(advanced.versions ?? [])])];
  let reason: string | undefined;
  if (Object.keys(patch).length === 0) reason = t("advanced.nothingChanged");
  else if (Object.keys(problems).length > 0) reason = t("quick.fixErrors");
  const policyDefault = advanced.restart_policy_default;
  return (
    <form
      data-testid="advanced-form"
      aria-label={t("tabs.advanced")}
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
      }}
    >
      <div className="flex items-center gap-2">
        <Toggle
          label={t("advanced.autostart")}
          checked={draft.autostart}
          onCheckedChange={(next) => {
            set("autostart", next);
          }}
        />
        <span className="text-callout text-fg-secondary" aria-hidden="true">
          {t("advanced.autostart")}
        </span>
      </div>
      <TextArea
        label={t("advanced.args")}
        hint={t("advanced.argsHint")}
        value={draft.args}
        onChange={(next) => {
          set("args", next);
        }}
      />
      <TextArea
        label={t("advanced.env")}
        hint={t("advanced.envHint")}
        value={draft.env}
        error={problems.env === true ? t("advanced.envInvalid") : undefined}
        onChange={(next) => {
          set("env", next);
        }}
      />
      <section aria-labelledby="advanced-restart" className="flex flex-col gap-3">
        <h3 id="advanced-restart" className="text-headline">
          {t("advanced.restartPolicy")}
        </h3>
        <div className="flex flex-col gap-1">
          <span className="text-callout text-fg-secondary" aria-hidden="true">
            {t("advanced.restartOn")}
          </span>
          <Select
            label={t("advanced.restartOn")}
            value={draft.on}
            options={RESTART_ON.map((value) => ({
              value,
              label:
                value === policyDefault.on
                  ? t("advanced.onDefault", { label: t(`advanced.on.${value}`) })
                  : t(`advanced.on.${value}`),
            }))}
            onValueChange={(next) => {
              set("on", next as AdvancedDraft["on"]);
            }}
          />
        </div>
        {draft.on === "failure" ? (
          <div className="flex gap-3">
            <TextField
              width="sm"
              label={t("advanced.restartMax")}
              inputMode="numeric"
              value={draft.max}
              {...(problems.max === true ? { error: t("advanced.positive") } : {})}
              onChange={(event) => {
                set("max", event.target.value);
              }}
            />
            <TextField
              width="sm"
              label={t("advanced.restartWindow")}
              inputMode="numeric"
              value={draft.windowSecs}
              {...(problems.window === true ? { error: t("advanced.positive") } : {})}
              onChange={(event) => {
                set("windowSecs", event.target.value);
              }}
            />
          </div>
        ) : null}
      </section>
      <div className="flex flex-col gap-1">
        <span className="text-callout text-fg-secondary" aria-hidden="true">
          {t("advanced.version")}
        </span>
        <Select
          label={t("advanced.version")}
          value={draft.version}
          options={versions.map((value) => ({ value, label: value }))}
          onValueChange={(next) => {
            set("version", next);
          }}
        />
        <p className="text-caption text-fg-tertiary">{t("advanced.versionHint")}</p>
      </div>
      <div className="flex flex-col gap-2">
        {Object.keys(patch).length === 0 ? null : (
          <p data-testid="apply-note" className="text-callout text-fg-secondary">
            {t("advanced.restartNote")}
          </p>
        )}
        <div>
          <ActionButton
            variant="primary"
            action={updateInstance}
            input={{ instance: target, patch }}
            loadingLabel={t("quick.saving")}
            successLabel={t("quick.saved")}
            {...(reason === undefined ? {} : { disabledReason: reason })}
          >
            {t("advanced.save")}
          </ActionButton>
        </div>
      </div>
    </form>
  );
}

/**
 * Avancado: extra args, environment, restart policy, autostart and a same-series version
 * (`services.update`, only the changed fields). The data is in `services.settings.advanced`,
 * which the daemon only fills for admin connections.
 */
export function AdvancedTab({ target, version }: AdvancedTabProps) {
  const { t } = useTranslation("services");
  const settings = useForgeQuery("services.settings", { target });
  return (
    <QueryBoundary query={settings} height={240}>
      {(dto) =>
        dto.advanced === null || dto.advanced === undefined ? (
          <p data-testid="advanced-unavailable" className="text-callout text-fg-secondary">
            {t("advanced.unavailable")}
          </p>
        ) : (
          <AdvancedForm
            key={JSON.stringify(dto.advanced)}
            target={target}
            version={version}
            advanced={dto.advanced}
          />
        )
      }
    </QueryBoundary>
  );
}
