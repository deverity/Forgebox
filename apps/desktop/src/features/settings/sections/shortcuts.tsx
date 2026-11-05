import { useState } from "react";
import type { KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { chordFromEvent, chordKeys, type ShortcutId } from "../../../app/shortcuts/registry";
import {
  describeShortcutError,
  useShortcutBindings,
  useShortcutRegistry,
} from "../../../app/shortcuts/useShortcuts";
import { Button } from "../../../design/primitives/Button";
import { Kbd } from "../../../design/primitives/Kbd";
import { loose } from "../looseT";
import { matchesSearch } from "../model";
import type { SectionProps } from "../registry";

const RESET_ALL_ID = "shortcuts-reset-all";

/**
 * Atalhos: an editor over the shortcut registry (docs/07 section 8). "Alterar" listens for the
 * next chord and asks the registry to bind it; a chord another command owns is refused with the
 * name of that command. "Restaurar" goes back to the default. Overrides persist in the GUI
 * prefs (`shortcuts`).
 */
export function ShortcutsSection({ query }: SectionProps) {
  const { t } = useTranslation("settings");
  const { t: tp } = useTranslation("palette");
  const registry = useShortcutRegistry();
  const bindings = useShortcutBindings();
  const [recording, setRecording] = useState<ShortcutId | null>(null);
  const [errors, setErrors] = useState<Readonly<Partial<Record<ShortcutId, string>>>>({});

  const report = (id: ShortcutId, message: string | undefined) => {
    setErrors((current) => {
      const rest = Object.fromEntries(Object.entries(current).filter(([key]) => key !== id));
      return message === undefined ? rest : { ...rest, [id]: message };
    });
  };

  const onKeyDown = (id: ShortcutId) => (event: KeyboardEvent<HTMLButtonElement>) => {
    if (recording !== id) return;
    // The window-level handler skips events that are already handled.
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape") {
      setRecording(null);
      return;
    }
    const chord = chordFromEvent(event.nativeEvent);
    if (chord === null) return;
    setRecording(null);
    void registry.set(id, chord).then((result) => {
      report(id, result.ok ? undefined : describeShortcutError(result.error, loose(tp)));
    });
  };

  const name = (id: ShortcutId) => loose(tp)(`shortcuts.names.${id}`);
  const visible = bindings.filter((b) => matchesSearch(query, name(b.id), b.chord));
  const anyOverride = bindings.some((b) => b.overridden);

  return (
    <div className="flex flex-col gap-2">
      <p className="text-callout text-fg-secondary">{t("shortcuts.intro")}</p>
      {anyOverride ? (
        <div>
          <Button
            data-testid={RESET_ALL_ID}
            onClick={() => {
              for (const b of bindings) if (b.overridden) void registry.reset(b.id);
              setErrors({});
            }}
          >
            {t("shortcuts.resetAll")}
          </Button>
        </div>
      ) : null}
      <ul className="flex flex-col">
        {visible.map((binding) => {
          const error = errors[binding.id];
          const isRecording = recording === binding.id;
          return (
            <li
              key={binding.id}
              data-testid={`shortcut-${binding.id}`}
              className="flex flex-wrap items-center justify-between gap-3 border-b border-stroke py-2"
            >
              <div className="flex flex-col">
                <span className="text-body">{name(binding.id)}</span>
                {binding.global ? (
                  <span className="text-caption text-fg-tertiary">{t("shortcuts.global")}</span>
                ) : null}
                {error === undefined ? null : (
                  <span
                    role="alert"
                    data-testid="shortcut-error"
                    className="text-callout text-danger"
                  >
                    {error}
                  </span>
                )}
              </div>
              <div className="flex items-center gap-2">
                <Button
                  data-testid={`shortcut-change-${binding.id}`}
                  aria-label={t("shortcuts.changeFor", { name: name(binding.id) })}
                  aria-pressed={isRecording}
                  onKeyDown={onKeyDown(binding.id)}
                  onBlur={() => {
                    if (isRecording) setRecording(null);
                  }}
                  onClick={() => {
                    report(binding.id, undefined);
                    setRecording(binding.id);
                  }}
                >
                  {isRecording ? (
                    t("shortcuts.press")
                  ) : (
                    <span className="flex items-center gap-1">
                      {chordKeys(binding.chord).map((key) => (
                        <Kbd key={key}>{key}</Kbd>
                      ))}
                    </span>
                  )}
                </Button>
                {binding.overridden ? (
                  <Button
                    variant="ghost"
                    data-testid={`shortcut-reset-${binding.id}`}
                    aria-label={t("shortcuts.resetFor", { name: name(binding.id) })}
                    onClick={() => {
                      void registry.reset(binding.id).then((result) => {
                        report(
                          binding.id,
                          result.ok ? undefined : describeShortcutError(result.error, loose(tp)),
                        );
                      });
                    }}
                  >
                    {t("shortcuts.reset")}
                  </Button>
                ) : null}
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
