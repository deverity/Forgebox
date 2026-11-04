import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../../../actions/registry";
import { setConfig } from "../../../../actions/system";
import { Button } from "../../../../design/primitives/Button";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { PortField } from "../../../../design/primitives/PortField";
import { TextField } from "../../../../design/primitives/TextField";
import { newIntent } from "../../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import { useCapability, useRuntime } from "../../../../state";
import { RowFrame } from "../../RowFrame";
import { parsePortSpec, sameSpec, type RangeEntry } from "./model";

const CAPTION_FORGE_CONFIG_SET_PORTS_RANGES_NAME_8080_8443 =
  "forge config set ports.ranges.<name> [8080,8443]";
const CHECK = "✓";
const METHOD_SYSTEM_CONFIG_SET = "system.config_set";
const TID_PORT_PROBE = "port-probe";

const SAVED_MS = 2000;

/** One named range (`ports.ranges.<name>`) as editable text; saves on Enter or "Salvar". */
function RangeRow({ entry }: { entry: RangeEntry }) {
  const { t } = useTranslation("settings");
  const runtime = useRuntime();
  const capability = useCapability("system.config_set");
  const [draft, setDraft] = useState<string | null>(null);
  const [phase, setPhase] = useState<"idle" | "saving" | "saved">("idle");
  const [error, setError] = useState<ForgeApiError | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  // Text just saved, shown until `system.config_list` brings the new value (no flicker back).
  const [settled, setSettled] = useState<{ spec: string; base: string } | null>(null);
  const alive = useRef(true);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (timer.current !== null) clearTimeout(timer.current);
    };
  }, []);

  const key = `ports.ranges.${entry.name}`;
  const baseline = settled !== null && settled.base === entry.spec ? settled.spec : entry.spec;
  const shown = draft ?? baseline;
  const dirty = draft !== null && !sameSpec(draft, baseline);
  const parsed = parsePortSpec(shown);

  const save = () => {
    if (!parsed.ok) {
      setProblem(t("network.ports.invalid"));
      return;
    }
    setProblem(null);
    setPhase("saving");
    setError(null);
    runAction(runtime.queue, setConfig, { key, value: parsed.value }, { intent: newIntent() }).then(
      () => {
        if (!alive.current) return;
        setDraft(null);
        setSettled({ spec: shown, base: entry.spec });
        setPhase("saved");
        timer.current = setTimeout(() => {
          if (alive.current) setPhase("idle");
        }, SAVED_MS);
      },
      (failure: unknown) => {
        if (!alive.current) return;
        setDraft(null);
        setPhase("idle");
        setError(toForgeApiError(failure, { method: METHOD_SYSTEM_CONFIG_SET }));
      },
    );
  };

  return (
    <li data-testid={`range-${entry.name}`} className="flex flex-col gap-2 py-2">
      <div className="flex flex-wrap items-end gap-2">
        <TextField
          label={entry.name}
          width="full"
          value={shown}
          disabled={capability.text !== null || phase === "saving"}
          hint={key}
          {...(problem === null ? {} : { error: problem })}
          onChange={(event) => {
            setDraft(event.target.value);
            setProblem(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && dirty) save();
          }}
        />
        <Button
          variant="primary"
          loading={phase === "saving"}
          loadingLabel={t("saving")}
          disabled={!dirty || capability.text !== null}
          onClick={save}
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
        {phase === "saved" ? (
          <span data-testid={`range-${entry.name}-saved`} className="text-callout">
            <span aria-hidden="true">{CHECK}</span> {t("saved")}
          </span>
        ) : null}
      </div>
      {error === null ? null : (
        <ErrorNotice error={error} context={{ method: METHOD_SYSTEM_CONFIG_SET }} />
      )}
    </li>
  );
}

/**
 * Port ranges (`[ports.ranges]`, docs/03 section 4): one text field per named range plus a
 * `PortField` that asks `net.port_check` whether a single port is free right now (who holds
 * it, or which Forgebox owner reserved it), so a range can be chosen without a conflict.
 */
export function PortsEditor({ ranges }: { ranges: readonly RangeEntry[] }) {
  const { t } = useTranslation("settings");
  const [probe, setProbe] = useState("");
  return (
    <RowFrame
      id="ports"
      label={t("network.ports.label")}
      description={t("network.ports.description")}
      caption={CAPTION_FORGE_CONFIG_SET_PORTS_RANGES_NAME_8080_8443}
    >
      <div className="flex w-full min-w-72 flex-col gap-2">
        <PortField
          label={t("network.ports.probe")}
          value={probe}
          onChange={setProbe}
          placeholder="8080"
          data-testid={TID_PORT_PROBE}
        />
        {ranges.length === 0 ? (
          <p className="text-callout text-fg-secondary">{t("network.ports.none")}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-stroke">
            {ranges.map((entry) => (
              <RangeRow key={entry.name} entry={entry} />
            ))}
          </ul>
        )}
      </div>
    </RowFrame>
  );
}
