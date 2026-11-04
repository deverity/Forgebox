import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { FIX_ALLOWLIST } from "../../actions/diagnostics";
import type { Action } from "../../actions/registry";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Badge, type BadgeTone } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import type * as T from "../../ipc/generated";
import { useCapability, useHello } from "../../state";
import { KillDialog } from "./KillDialog";
import { canFix, copyableFix, portOf } from "./model";

const GLYPH: Record<T.CheckStatus, string> = { ok: "✓", warn: "!", fail: "✕", skipped: "–" };
const TONE: Record<T.CheckStatus, BadgeTone> = {
  ok: "success",
  warn: "warning",
  fail: "danger",
  skipped: "neutral",
};
const SERVICES_PATH = "/services";

export interface CheckRowProps {
  check: T.CheckDto;
  /** The row's group is being checked again. */
  busy: boolean;
  /** Re-runs the check's group (after a fix or a kill). */
  onRerun: () => void;
}

/** One check: status icon and text, and what can be done about it (docs/07 section 5.12). */
export function CheckRow({ check, busy, onRerun }: CheckRowProps) {
  const { t } = useTranslation("diagnostics");
  const navigate = useNavigate();
  const kill = useCapability("services.kill_port_holder");
  const hello = useHello();
  // `services.kill_port_holder` is local only: on a remote daemon the button does not exist.
  const remote = hello !== null && !hello.local;
  const [killing, setKilling] = useState(false);
  const port = portOf(check);
  const conflict = port !== null && check.status !== "ok";
  const fixable = canFix(check);
  const entry =
    fixable && typeof check.fix_method === "string" ? FIX_ALLOWLIST[check.fix_method] : undefined;
  const copy = copyableFix(check);
  return (
    <li
      data-check={check.id}
      data-status={check.status}
      aria-busy={busy || undefined}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-start gap-3">
        <span aria-hidden="true" className="mt-0.5 w-4 text-center text-callout text-fg-secondary">
          {GLYPH[check.status]}
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <code className="font-mono text-mono text-fg-secondary">{check.id}</code>
            <Badge tone={TONE[check.status]}>{t(`status.${check.status}`)}</Badge>
            {busy ? <span className="text-caption text-fg-tertiary">{t("rechecking")}</span> : null}
          </div>
          <p className="mt-1 text-callout text-fg-primary [overflow-wrap:anywhere]">
            {check.message}
          </p>
        </div>
        <div className="flex shrink-0 flex-wrap justify-end gap-2">
          {entry !== undefined ? (
            <ActionButton
              action={entry.action as unknown as Action}
              input={entry.input(check.fix_params)}
              variant="primary"
              loadingLabel={t("fixing")}
              successLabel={t("fixed")}
              onDone={onRerun}
            >
              {t("fix")}
            </ActionButton>
          ) : null}
          {conflict && !remote ? (
            <Button
              variant="destructive"
              disabled={!kill.available}
              onClick={() => {
                setKilling(true);
              }}
            >
              {t("kill.action")}
            </Button>
          ) : null}
          {conflict ? (
            <Button
              variant="secondary"
              onClick={() => {
                void navigate({ to: SERVICES_PATH });
              }}
            >
              {t("changePort")}
            </Button>
          ) : null}
        </div>
      </div>
      {!fixable && check.status !== "ok" && copy !== null ? <CliCommand command={copy} /> : null}
      {port !== null ? (
        <KillDialog
          port={killing ? port : null}
          onClose={() => {
            setKilling(false);
          }}
          onKilled={onRerun}
        />
      ) : null}
    </li>
  );
}
