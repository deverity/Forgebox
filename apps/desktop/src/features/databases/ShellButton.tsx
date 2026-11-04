import { useNavigate } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { databaseShell } from "../../actions/databases";
import { ActionButton } from "../../design/primitives/ActionButton";
import type * as T from "../../ipc/generated";
import { useRuntime } from "../../state";
import { openTerminal } from "../../state/terminals";

const TERMINAL_PATH = "/terminal";

/**
 * "Abrir shell": `db.shell` answers the client's command line (local only; the answer is for the
 * caller's machine), which opens as a command tab of the integrated terminal (docs/07 §5.10).
 * The argv is `[program, ...args]`; the credentials the daemon puts in `env` travel to the
 * terminal session and are never shown.
 */
export function ShellButton({
  instance,
  database,
  title,
}: {
  instance: string;
  database: string | null;
  title: string;
}) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const navigate = useNavigate();
  return (
    <ActionButton
      action={databaseShell}
      input={{ instance, database }}
      loadingLabel={t("shell.loading")}
      onDone={(answer) => {
        const shell = answer as T.ShellCommandDto;
        openTerminal(
          {
            command: [shell.program, ...(shell.args ?? [])],
            title,
            ...(shell.cwd === null || shell.cwd === undefined ? {} : { dir: shell.cwd }),
            ...(shell.env === undefined ? {} : { env: shell.env }),
          },
          runtime.context,
        );
        void navigate({ to: TERMINAL_PATH });
      }}
    >
      {t("shell.action")}
    </ActionButton>
  );
}
