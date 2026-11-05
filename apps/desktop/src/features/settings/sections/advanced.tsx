import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../../design/primitives/Button";
import { copyText, revealPath } from "../../../ipc/native";
import { useHello } from "../../../state";
import { loose, useRoot } from "../looseT";
import { matchesSearch } from "../model";
import type { SectionProps } from "../registry";
import { RowFrame } from "../RowFrame";

/** Row ids of the section (`advanced.<id>.label` in the `settings` namespace). */
const DASH = "—";
const LOGS_PATH = "/logs";
const DIAGNOSTICS_PATH = "/diagnostics";

export const ADVANCED_ROWS = ["templates", "logs", "diagnostics"] as const;

/** Where a user template replaces a built-in one (docs/02 section 6: `<root>/etc/templates/`). */
export function templatesPath(root: string): string {
  return `${root.replace(/[\\/]+$/, "")}/etc/templates`;
}

const LINK_CLASS =
  "inline-flex h-8 items-center rounded-control border border-stroke-strong px-3 text-body text-fg-primary hover:bg-surface-2 outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent";

/**
 * Avancado: the templates override folder (read-only, derived from the root), and links to the
 * daemon logs and to the diagnostics. No `forgebox.toml` key lives here.
 */
export function AdvancedSection({ query }: SectionProps) {
  const { t } = useTranslation("settings");
  const hello = useHello();
  const root = useRoot();
  const [copied, setCopied] = useState(false);
  const path = templatesPath(root);
  const show = (id: (typeof ADVANCED_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`advanced.${id}.label`));

  return (
    <div className="flex flex-col">
      {show("templates") ? (
        <RowFrame
          id="templates"
          label={t("advanced.templates.label")}
          description={t("advanced.templates.description")}
        >
          <code data-testid="templates-path" className="text-body">
            {root === "" ? DASH : path}
          </code>
          {hello?.local === true ? (
            <Button
              onClick={() => {
                void revealPath(path);
              }}
            >
              {t("advanced.templates.open")}
            </Button>
          ) : null}
          <Button
            disabled={root === ""}
            onClick={() => {
              void copyText(path).then(setCopied);
            }}
          >
            {copied ? t("advanced.templates.copied") : t("advanced.templates.copy")}
          </Button>
        </RowFrame>
      ) : null}
      {show("logs") ? (
        <RowFrame
          id="logs"
          label={t("advanced.logs.label")}
          description={t("advanced.logs.description")}
          caption="forge daemon logs"
        >
          <Link to={LOGS_PATH} className={LINK_CLASS}>
            {t("advanced.logs.open")}
          </Link>
        </RowFrame>
      ) : null}
      {show("diagnostics") ? (
        <RowFrame
          id="diagnostics"
          label={t("advanced.diagnostics.label")}
          description={t("advanced.diagnostics.description")}
          caption="forge doctor"
        >
          <Link to={DIAGNOSTICS_PATH} className={LINK_CLASS}>
            {t("advanced.diagnostics.open")}
          </Link>
        </RowFrame>
      ) : null}
    </div>
  );
}
