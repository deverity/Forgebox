import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { copyText } from "../../ipc/native";
import { Button } from "./Button";

export interface CliCommandProps {
  /** The `forge …` command (from `renderCli`); `null` renders nothing. Never translated. */
  command: string | null;
}

const COPIED_MS = 1500;

/**
 * The equivalent `forge` command in mono with "Copiar comando" (docs/07 §1.7). Used in Sheets
 * and detail panels; tooltips use the `command` slot of `Tooltip`. `data-forge-cli` carries the
 * command for the E4-T5 parity gate.
 */
export function CliCommand({ command }: CliCommandProps) {
  const { t } = useTranslation("common");
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );
  if (command === null) return null;
  const copy = async () => {
    const ok = await copyText(command);
    setState(ok ? "copied" : "failed");
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      setState("idle");
    }, COPIED_MS);
  };
  return (
    <div
      data-forge-cli={command}
      className="flex items-center gap-2 rounded-control border border-stroke bg-surface px-2 py-1"
    >
      <code className="min-w-0 flex-1 truncate font-mono text-mono text-fg-secondary">
        {command}
      </code>
      <Button
        variant="ghost"
        onClick={() => {
          void copy();
        }}
      >
        {state === "copied"
          ? t("ui.cliCopied")
          : state === "failed"
            ? t("ui.cliCopyFailed")
            : t("ui.cliCopy")}
      </Button>
    </div>
  );
}
