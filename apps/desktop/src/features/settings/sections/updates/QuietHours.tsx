import { useState } from "react";
import { useTranslation } from "react-i18next";
import { setConfig } from "../../../../actions/system";
import { ActionButton } from "../../../../design/primitives/ActionButton";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { TextField } from "../../../../design/primitives/TextField";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";
import { quietOf, quietProblem, quietValue } from "./model";

const KEY_QUIET = "updates.quiet_hours";

/**
 * Quiet hours (`updates.quiet_hours`): background update checks and downloads wait outside the
 * window. Both times `HH:MM` or both empty; the pair is validated here and by the daemon.
 */
export function QuietHours({ values }: { values: unknown }) {
  const { t } = useTranslation("settings");
  const saved = quietOf(values);
  const [draft, setDraft] = useState<{ start: string; end: string } | null>(null);
  const shown = draft ?? saved;
  const problem = quietProblem(shown.start, shown.end);
  const message = problem === undefined ? undefined : loose(t)(`updates.quiet.problem.${problem}`);
  const input = { key: KEY_QUIET, value: quietValue(shown.start, shown.end) };
  const dirty = draft !== null && (draft.start !== saved.start || draft.end !== saved.end);
  const reason = dirty ? message : t("updates.quiet.unchanged");
  return (
    <RowFrame
      id="update-quiet"
      label={t("updates.quiet.label")}
      description={t("updates.quiet.description")}
    >
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-start gap-3">
          <TextField
            label={t("updates.quiet.start")}
            width="sm"
            inputMode="numeric"
            placeholder="22:00"
            value={shown.start}
            {...(dirty && message !== undefined ? { error: message } : {})}
            onChange={(event) => {
              setDraft({ ...shown, start: event.target.value });
            }}
          />
          <TextField
            label={t("updates.quiet.end")}
            width="sm"
            inputMode="numeric"
            placeholder="07:00"
            value={shown.end}
            {...(dirty && message !== undefined ? { error: message } : {})}
            onChange={(event) => {
              setDraft({ ...shown, end: event.target.value });
            }}
          />
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <ActionButton
            action={setConfig}
            input={input}
            variant="primary"
            {...(reason === undefined ? {} : { disabledReason: reason })}
            onDone={() => {
              setDraft(null);
            }}
          >
            {t("save")}
          </ActionButton>
          <CliCommand command={setConfig.cli(input)} />
        </div>
      </div>
    </RowFrame>
  );
}
