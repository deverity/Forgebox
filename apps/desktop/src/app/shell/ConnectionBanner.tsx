import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { SPINNER_INLINE } from "../../design/primitives/shared";
import { CLIENT_COMMANDS } from "../../ipc/describeError";
import { useErrorMessage } from "./useErrorText";
import { toast } from "../../design/primitives/toastStore";
import { useConnection, useRuntime } from "../../state";

function Busy({ children }: { children: string }) {
  // A spinner never stands alone: the text says what is going on (docs/07 section 11).
  return (
    <>
      <span aria-hidden="true" className={SPINNER_INLINE} />
      <span>{children}</span>
    </>
  );
}

/**
 * Connection state above the content (docs/07 section 4): connecting, reconnecting, starting
 * the daemon, or stopped on purpose with "Iniciar" and its `forge daemon start` equivalent.
 * Nothing when connected (or when the root is not set up yet: onboarding owns that case).
 * Every route stays navigable while it shows.
 */
export function ConnectionBanner() {
  const { t } = useTranslation("shell");
  const errorMessage = useErrorMessage();
  const runtime = useRuntime();
  const status = useConnection((s) => s.status);
  const attempt = useConnection((s) => s.state?.attempt ?? 0);
  const [starting, setStarting] = useState(false);

  const start = () => {
    setStarting(true);
    runtime.api
      .start()
      .catch((error: unknown) => {
        toast.show({ kind: "error", message: errorMessage(error) });
      })
      .finally(() => {
        setStarting(false);
      });
  };

  let body;
  switch (status) {
    case "connecting":
      body = <Busy>{t("banner.connecting")}</Busy>;
      break;
    case "starting":
      body = <Busy>{t("banner.starting")}</Busy>;
      break;
    case "reconnecting":
      body = (
        <Busy>
          {attempt > 1 ? t("banner.reconnectingAttempt", { attempt }) : t("banner.reconnecting")}
        </Busy>
      );
      break;
    case "stopped":
      body = (
        <>
          <span>{t("banner.stopped")}</span>
          <Button
            variant="primary"
            loading={starting}
            loadingLabel={t("banner.startingAction")}
            onClick={start}
          >
            {t("banner.start")}
          </Button>
          <span className="text-fg-secondary">{t("banner.orRun")}</span>
          <code className="rounded-control border border-stroke bg-surface px-2 font-mono text-mono">
            {CLIENT_COMMANDS.start_daemon}
          </code>
        </>
      );
      break;
    default:
      return null;
  }
  return (
    <div
      role="status"
      data-testid="connection-banner"
      data-status={status}
      className="flex shrink-0 flex-wrap items-center gap-3 border-b border-stroke bg-surface-2 px-4 py-2 text-body text-fg-primary"
    >
      {body}
    </div>
  );
}
