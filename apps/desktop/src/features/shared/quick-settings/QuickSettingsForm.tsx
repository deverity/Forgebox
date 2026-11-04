import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { setServiceSettings } from "../../../actions/services";
import { ActionButton } from "../../../design/primitives/ActionButton";
import type { PortCheckDto, QuickSettingDto, ServicePortSettingDto } from "../../../ipc/generated";
import { QuickSettingField, problemText } from "./QuickSettingField";
import {
  applyOf,
  baselineOf,
  changedSettings,
  dirtyKeys,
  portKey,
  resetTarget,
  validateSetting,
  type Draft,
} from "./model";

export interface QuickSettingsFormProps {
  /** Target of `services.set` (`mysql/default`, `php@8.4`). */
  target: string;
  settings: readonly QuickSettingDto[];
  /** Named ports (instances); the pools of a PHP series have none. */
  ports?: readonly ServicePortSettingDto[];
  /** The changes were applied. */
  onSaved?: () => void;
}

/** Who holds a port that a check says is not free. */
function ownerOf(result: PortCheckDto): string {
  return result.reserved_by ?? result.holder?.exe ?? "?";
}

/**
 * The generic quick-settings form (docs/07 §5.3), built only from `ServiceSettingsDto`: eight
 * control kinds, inline validation, dirty tracking and a per-field reset. Submit sends just
 * the changed keys through `services.set` and says whether that means a reload or a restart.
 * A new port that the allocator or a process already holds is refused before submit.
 * The caller remounts it (`key`) when the saved state changes.
 */
export function QuickSettingsForm({
  target,
  settings,
  ports = [],
  onSaved,
}: QuickSettingsFormProps) {
  const { t } = useTranslation("services");
  const baseline = useMemo(() => baselineOf(settings, ports), [settings, ports]);
  const [draft, setDraft] = useState<Draft>(baseline);
  const [conflicts, setConflicts] = useState<Readonly<Record<string, string>>>({});

  const dirty = dirtyKeys(baseline, draft);
  const problems: Record<string, string> = {};
  for (const setting of settings) {
    if (!dirty.includes(setting.key)) continue;
    const problem = validateSetting(setting, draft[setting.key] ?? "");
    if (problem !== undefined) problems[setting.key] = problemText(t, problem);
  }
  for (const port of ports) {
    const key = portKey(port.name);
    if (!dirty.includes(key)) continue;
    const text = draft[key] ?? "";
    if (!/^\d{1,5}$/.test(text) || Number(text) < 1 || Number(text) > 65535) {
      problems[key] = t("quick.problem.port");
    }
  }
  // A conflict only counts for a port the user changed.
  const conflictOf = (key: string): string | undefined =>
    dirty.includes(key) ? conflicts[key] : undefined;
  const conflictKeys = dirty.filter((key) => conflictOf(key) !== undefined);

  const set = (key: string, text: string) => {
    setDraft((current) => ({ ...current, [key]: text }));
  };
  const reportPort = (key: string) => (result: PortCheckDto | null) => {
    const owner = result === null || result.free ? undefined : ownerOf(result);
    setConflicts((current) => {
      if (current[key] === owner) return current;
      const rest = Object.entries(current).filter(([other]) => other !== key);
      return Object.fromEntries(owner === undefined ? rest : [...rest, [key, owner]]);
    });
  };

  let reason: string | undefined;
  if (dirty.length === 0) reason = t("quick.nothingChanged");
  else if (Object.keys(problems).length > 0) reason = t("quick.fixErrors");
  else if (conflictKeys.length > 0) {
    const key = conflictKeys[0] ?? "";
    reason = t("quick.portConflict", { owner: conflictOf(key) ?? "" });
  }

  const apply = applyOf(settings, dirty);
  const changed = changedSettings(baseline, draft);
  // The button is disabled while nothing changed, but its tooltip and `data-forge-cli` still show
  // the command shape: the first key at its saved value (never run, the button is disabled).
  const firstKey = Object.keys(baseline)[0];
  const preview =
    dirty.length === 0 && firstKey !== undefined
      ? { [firstKey]: baseline[firstKey] ?? "" }
      : changed;

  return (
    <form
      data-testid="quick-settings"
      aria-label={t("quick.title")}
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
      }}
    >
      {settings.length === 0 && ports.length === 0 ? (
        <p className="text-callout text-fg-secondary">{t("quick.empty")}</p>
      ) : null}
      {settings.map((setting) => {
        const error = problems[setting.key];
        return (
          <QuickSettingField
            key={setting.key}
            setting={setting}
            value={draft[setting.key] ?? ""}
            onChange={(text) => {
              set(setting.key, text);
            }}
            error={error}
            conflict={conflictOf(setting.key) !== undefined}
            reset={(() => {
              const back = resetTarget(setting, baseline, draft);
              return back === null
                ? null
                : {
                    kind: back.kind,
                    onClick: () => {
                      set(setting.key, back.text);
                    },
                  };
            })()}
            onPortResult={reportPort(setting.key)}
          />
        );
      })}
      {ports.length === 0 ? null : (
        <section aria-labelledby="quick-ports" className="flex flex-col gap-3">
          <h3 id="quick-ports" className="text-headline">
            {t("quick.ports")}
          </h3>
          {ports.map((port) => {
            const key = portKey(port.name);
            const saved = baseline[key] ?? "";
            const conflict = conflictOf(key);
            const error =
              problems[key] ??
              (conflict === undefined ? undefined : t("quick.portConflict", { owner: conflict }));
            return (
              <QuickSettingField
                key={key}
                setting={{ key, label: key, kind: "port", apply: "restart" }}
                value={draft[key] ?? ""}
                onChange={(text) => {
                  set(key, text);
                }}
                error={error}
                conflict={conflict !== undefined}
                reset={
                  dirty.includes(key)
                    ? {
                        kind: "undo",
                        onClick: () => {
                          set(key, saved);
                        },
                      }
                    : null
                }
                onPortResult={reportPort(key)}
              />
            );
          })}
        </section>
      )}
      <div className="flex flex-col gap-2">
        {apply === null ? null : (
          <p data-testid="apply-note" data-apply={apply} className="text-callout text-fg-secondary">
            {t(`quick.apply.${apply}`)}
          </p>
        )}
        <div>
          <ActionButton
            variant="primary"
            action={setServiceSettings}
            input={{ target, settings: preview }}
            loadingLabel={t("quick.saving")}
            successLabel={t("quick.saved")}
            {...(reason === undefined ? {} : { disabledReason: reason })}
            onDone={() => {
              onSaved?.();
            }}
          >
            {t("quick.save")}
          </ActionButton>
        </div>
      </div>
    </form>
  );
}
