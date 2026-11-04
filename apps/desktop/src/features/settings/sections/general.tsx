import { useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { applyAppearance, isTauri } from "../../../app/appearance";
import { Button } from "../../../design/primitives/Button";
import { SegmentedControl } from "../../../design/primitives/SegmentedControl";
import { Toggle } from "../../../design/primitives/Toggle";
import { LanguageSwitcher } from "../../../i18n/LanguageSwitcher";
import { revealPath } from "../../../ipc/native";
import { useHello } from "../../../state";
import { AutostartToggle } from "../../updater";
import {
  GUI_PREF_DEFAULTS,
  THEME_PREFS,
  defaultGuiPrefs,
  type GuiPrefsValues,
  type ThemePref,
} from "../guiPrefs";
import { loose, useRoot } from "../looseT";
import { matchesSearch } from "../model";
import type { SectionProps } from "../registry";
import { RowFrame } from "../RowFrame";

const DASH = "—";
const ONBOARDING_PATH = "/onboarding";

/** Row ids of the section (`general.<id>.label` in the `settings` namespace). */
export const GENERAL_ROWS = [
  "root",
  "language",
  "theme",
  "closeToTray",
  "autostart",
  "onboarding",
] as const;

/**
 * Geral: the root folder (read-only), language, theme, close to tray, Windows autostart and
 * "run onboarding again". Everything but the root is a GUI-local preference: it goes to
 * `gui_prefs_set` (or the autostart command), never to `system.config_set`.
 */
export function GeneralSection({ query }: SectionProps) {
  const { t } = useTranslation("settings");
  const hello = useHello();
  const root = useRoot();
  const navigate = useNavigate();
  const [prefs, setPrefs] = useState<GuiPrefsValues>(GUI_PREF_DEFAULTS);
  const [failure, setFailure] = useState<string | null>(null);
  const [opened, setOpened] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    defaultGuiPrefs()
      .get()
      .then(
        (value) => {
          if (!cancelled) setPrefs(value);
        },
        () => undefined,
      );
    return () => {
      cancelled = true;
    };
  }, []);

  const update = (patch: Partial<GuiPrefsValues>) => {
    const previous = prefs;
    setPrefs({ ...prefs, ...patch });
    setFailure(null);
    defaultGuiPrefs()
      .set(patch)
      .then(
        () => {
          // The Rust side applies the theme through the appearance event; a plain browser
          // (QA build) has no such event.
          if (patch.theme !== undefined && patch.theme !== "system" && !isTauri()) {
            applyAppearance({ theme: patch.theme });
          }
        },
        (error: unknown) => {
          setPrefs(previous);
          setFailure(error instanceof Error ? error.message : String(error));
        },
      );
  };

  const show = (id: (typeof GENERAL_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`general.${id}.label`));
  const local = hello?.local === true;

  return (
    <div className="flex flex-col">
      {failure === null ? null : (
        <p role="alert" data-testid="prefs-error" className="text-callout text-danger">
          {t("general.saveFailed", { message: failure })}
        </p>
      )}
      {show("root") ? (
        <RowFrame
          id="root"
          label={t("general.root.label")}
          description={t("general.root.description")}
        >
          <code data-testid="root-path" className="text-body">
            {root === "" ? DASH : root}
          </code>
          {local ? (
            <Button
              disabled={root === ""}
              onClick={() => {
                void revealPath(root).then(setOpened);
              }}
            >
              {t("general.root.open")}
            </Button>
          ) : null}
          {opened === false ? (
            <span className="text-caption text-fg-tertiary">{t("general.root.openFailed")}</span>
          ) : null}
        </RowFrame>
      ) : null}
      {show("language") ? (
        <RowFrame
          id="language"
          label={t("general.language.label")}
          description={t("general.language.description")}
        >
          <LanguageSwitcher />
        </RowFrame>
      ) : null}
      {show("theme") ? (
        <RowFrame id="theme" label={t("general.theme.label")}>
          <SegmentedControl
            label={t("general.theme.label")}
            value={prefs.theme}
            options={THEME_PREFS.map((value) => ({
              value,
              label: loose(t)(`general.theme.${value}`),
            }))}
            onValueChange={(value) => {
              update({ theme: value as ThemePref });
            }}
          />
        </RowFrame>
      ) : null}
      {show("closeToTray") ? (
        <RowFrame
          id="close-to-tray"
          label={t("general.closeToTray.label")}
          description={t("general.closeToTray.description")}
        >
          <Toggle
            label={t("general.closeToTray.label")}
            checked={prefs.close_to_tray}
            onCheckedChange={(value) => {
              update({ close_to_tray: value });
            }}
          />
        </RowFrame>
      ) : null}
      {show("autostart") ? (
        <RowFrame id="autostart" label={t("general.autostart.label")}>
          <AutostartToggle showLabel={false} />
        </RowFrame>
      ) : null}
      {show("onboarding") ? (
        <RowFrame
          id="onboarding"
          label={t("general.onboarding.label")}
          description={t("general.onboarding.description")}
        >
          <Button
            onClick={() => {
              void navigate({ to: ONBOARDING_PATH });
            }}
          >
            {t("general.onboarding.action")}
          </Button>
        </RowFrame>
      ) : null}
    </div>
  );
}
