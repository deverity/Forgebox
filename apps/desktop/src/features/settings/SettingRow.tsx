import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { restartDaemon, rotateCa, setConfig } from "../../actions/system";
import { enableDns } from "../../actions/diagnostics";
import { runAction } from "../../actions/registry";
import { Button } from "../../design/primitives/Button";
import { ActionButton } from "../../design/primitives/ActionButton";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice, useErrorTranslate } from "../../design/primitives/ErrorNotice";
import { Select } from "../../design/primitives/Select";
import { TextField } from "../../design/primitives/TextField";
import { Toggle } from "../../design/primitives/Toggle";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type { ConfigValueDto } from "../../ipc/generated";
import { useCapability, useRuntime } from "../../state";
import { loose } from "./looseT";
import { encode, sameValue, toDraft, type Preset, type RowSpec } from "./model";

/** Select values cannot be empty: this stands for "unset". */
const UNSET = "__unset__";
const SAVED_MS = 2000;
const CHECK = "✓";
const SET_METHOD = "system.config_set";

export interface SettingRowProps {
  spec: RowSpec;
  /** Localized label; defaults to the schema title. */
  label?: string;
  /** Localized description; defaults to the schema description. */
  description?: string;
  /** Effective value from `system.config_list`. */
  value: unknown;
  /** Presets that fill the field (the user still saves). */
  presets?: readonly Preset[];
  /** Localized names of the presets by id. */
  presetLabel?: (id: string) => string;
  /** Localized name of a `select` option (default: the raw schema value). */
  optionLabel?: (option: string) => string;
  /** Extra controls on the right (the "Test" button). */
  extra?: ReactNode;
  /** Tells the section whether the field has unsaved changes. */
  onDirtyChange?: (dirty: boolean) => void;
}

/** Follow-ups (`ConfigValueDto.requires`) the GUI can run itself, as inline buttons. */
function Requires({ methods }: { methods: readonly string[] }) {
  const { t } = useTranslation("settings");
  const [rotating, setRotating] = useState(false);
  if (methods.length === 0) return null;
  return (
    <ul data-testid="setting-requires" className="flex flex-col gap-2">
      {methods.map((method) => (
        <li key={method} className="flex flex-wrap items-center gap-2 text-callout">
          <span className="text-fg-secondary">{loose(t)(`requires.${methodKey(method)}`)}</span>
          {method === "daemon.restart" ? (
            <ActionButton action={restartDaemon}>
              {t("requires.daemon_restart_action")}
            </ActionButton>
          ) : method === "net.dns_enable" ? (
            <ActionButton action={enableDns}>{t("requires.net_dns_enable_action")}</ActionButton>
          ) : method === "tls.ca_rotate" ? (
            rotating ? (
              <>
                <span className="text-fg-secondary">{t("requires.tls_ca_rotate_confirm")}</span>
                <ActionButton action={rotateCa} variant="destructive">
                  {t("requires.tls_ca_rotate_action")}
                </ActionButton>
              </>
            ) : (
              <Button
                onClick={() => {
                  setRotating(true);
                }}
              >
                {t("requires.tls_ca_rotate_action")}
              </Button>
            )
          ) : null}
        </li>
      ))}
    </ul>
  );
}

function methodKey(method: string): string {
  const known = ["daemon.restart", "net.dns_enable", "tls.ca_rotate"];
  return known.includes(method) ? method.replace(/\./g, "_") : "other";
}

/**
 * One configuration key as a row (docs/07 §5.11): label, description and the control picked
 * from the schema. Toggles and selects save on change; text, number and list fields save on
 * Enter or "Salvar". The row shows loading, success and the cause of a failure inline and,
 * when the daemon refuses (`config_invalid`), goes back to the value it holds. `requires`
 * of the answer appears as inline follow-up actions. A `config.changed` from another client
 * refetches `system.config_list`, so `value` changes under it.
 */
export function SettingRow({
  spec,
  label,
  description,
  value,
  presets,
  presetLabel,
  optionLabel,
  extra,
  onDirtyChange,
}: SettingRowProps) {
  const { t } = useTranslation("settings");
  const translate = useErrorTranslate();
  const runtime = useRuntime();
  const capability = useCapability("system.config_set");
  const uid = useId();
  const [draft, setDraft] = useState<string | null>(null);
  const [phase, setPhase] = useState<"idle" | "saving" | "saved">("idle");
  const [error, setError] = useState<ForgeApiError | null>(null);
  // Value just saved, shown until `system.config_list` brings the new one (no flicker back).
  const [settled, setSettled] = useState<{ value: unknown; base: string } | null>(null);
  const [requires, setRequires] = useState<readonly string[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const alive = useRef(true);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (timer.current !== null) clearTimeout(timer.current);
    };
  }, []);

  // `settled` only counts until the list brings another value (its key then differs).
  const valueKey = JSON.stringify(value ?? null);
  const baseline = settled !== null && settled.base === valueKey ? settled.value : value;

  const shown = draft ?? toDraft(spec, baseline);
  const dirty = draft !== null && draft !== toDraft(spec, baseline);
  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  const disabledReason =
    capability.text === null ? undefined : translate(capability.text.key, capability.text.values);
  const title = label ?? spec.title;
  const hint = description ?? spec.description;

  const save = (text: string) => {
    const encoded = encode(spec, text);
    if (!encoded.ok) {
      setProblem(
        encoded.problem === "range"
          ? t("problem.range", { min: spec.min ?? "", max: spec.max ?? "" })
          : t("problem.number"),
      );
      return;
    }
    setProblem(null);
    if (sameValue(encoded.value, baseline)) {
      setDraft(null);
      return;
    }
    setPhase("saving");
    setError(null);
    setRequires([]);
    runAction(
      runtime.queue,
      setConfig,
      { key: spec.key, value: encoded.value },
      {
        intent: newIntent(),
      },
    ).then(
      (result) => {
        if (!alive.current) return;
        setDraft(null);
        setSettled({ value: encoded.value, base: valueKey });
        setRequires((result as ConfigValueDto).requires ?? []);
        setPhase("saved");
        timer.current = setTimeout(() => {
          if (alive.current) setPhase("idle");
        }, SAVED_MS);
      },
      (failure: unknown) => {
        if (!alive.current) return;
        // The daemon kept the old value: show it again and say why.
        setDraft(null);
        setError(toForgeApiError(failure, { method: SET_METHOD }));
        setPhase("idle");
      },
    );
  };

  const busy = phase === "saving";
  const off = disabledReason !== undefined || busy;
  let control: ReactNode;
  if (spec.control === "toggle") {
    const checked = (draft ?? String(baseline === true)) === "true";
    control = (
      <Toggle
        id={uid}
        label={title}
        checked={checked}
        disabled={off}
        onCheckedChange={(next) => {
          setDraft(String(next));
          save(String(next));
        }}
      />
    );
  } else if (spec.control === "select") {
    const options = spec.options.map((option) => ({
      value: option === "" ? UNSET : option,
      label: option === "" ? t("auto") : (optionLabel?.(option) ?? option),
    }));
    control = (
      <Select
        id={uid}
        label={title}
        options={options}
        value={shown === "" ? UNSET : shown}
        disabled={off}
        onValueChange={(next) => {
          const text = next === UNSET ? "" : next;
          setDraft(text);
          save(text);
        }}
      />
    );
  } else {
    control = (
      <div className="flex flex-wrap items-end gap-2">
        <TextField
          label={title}
          width="full"
          value={shown}
          disabled={off}
          {...(spec.control === "number" && { inputMode: "numeric" as const })}
          {...(problem !== null && { error: problem })}
          {...(spec.control === "list" && { hint: t("listHint") })}
          onChange={(event) => {
            setDraft(event.target.value);
            setProblem(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && dirty) save(shown);
          }}
        />
        <Button
          variant="primary"
          loading={busy}
          loadingLabel={t("saving")}
          disabled={!dirty || off}
          onClick={() => {
            save(shown);
          }}
        >
          {t("save")}
        </Button>
        {dirty ? (
          <Button
            variant="ghost"
            onClick={() => {
              setDraft(null);
              setProblem(null);
            }}
          >
            {t("discard")}
          </Button>
        ) : null}
      </div>
    );
  }

  const commandValue = encode(spec, shown);
  const cli = setConfig.cli({ key: spec.key, value: commandValue.ok ? commandValue.value : shown });
  const showCli = dirty || phase !== "idle" || error !== null;
  const presetControl =
    presets === undefined ? null : (
      <div
        className="flex flex-wrap items-center gap-1"
        role="group"
        aria-label={t("presetsLabel")}
      >
        {presets.map((preset) => (
          <Button
            key={preset.id}
            variant="ghost"
            disabled={off}
            data-testid={`preset-${spec.key}-${preset.id}`}
            onClick={() => {
              setDraft(preset.command);
              setProblem(null);
            }}
          >
            {presetLabel?.(preset.id) ?? preset.id}
          </Button>
        ))}
      </div>
    );

  return (
    <div
      data-testid={`setting-${spec.key}`}
      data-control={spec.control}
      className="flex flex-col gap-2 border-b border-stroke py-3"
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex min-w-48 flex-1 flex-col gap-1">
          {spec.control === "toggle" || spec.control === "select" ? (
            <label htmlFor={uid} className="text-body">
              {title}
            </label>
          ) : null}
          {hint === "" ? null : <p className="text-callout text-fg-secondary">{hint}</p>}
          <code className="text-caption text-fg-tertiary">{spec.key}</code>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {control}
          {extra}
        </div>
      </div>
      {presetControl}
      <div aria-live="polite" className="text-callout">
        {phase === "saving" ? (
          <span data-testid="setting-saving" className="text-fg-secondary">
            {t("saving")}
          </span>
        ) : null}
        {phase === "saved" ? (
          <span data-testid="setting-saved" className="text-fg-primary">
            <span aria-hidden="true">{CHECK}</span>
            {t("saved")}
          </span>
        ) : null}
      </div>
      {error === null ? null : (
        <div data-testid="setting-error">
          <ErrorNotice error={error} context={{ method: SET_METHOD }} />
          {error.code === "config_invalid" || error.code === "invalid_input" ? (
            <p data-testid="setting-error-cause" className="text-callout text-fg-secondary">
              {error.message}
            </p>
          ) : null}
        </div>
      )}
      <Requires methods={requires} />
      {showCli ? <CliCommand command={cli} /> : null}
      {disabledReason === undefined ? null : (
        <p className="text-caption text-fg-tertiary">{disabledReason}</p>
      )}
    </div>
  );
}
