import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { IconButton } from "../../design/primitives/IconButton";
import { toast } from "../../design/primitives/toastStore";
import { useCapability, useRuntime } from "../../state";
import { useCapabilityReason, useErrorMessage } from "./useErrorText";

function PlayGlyph() {
  return (
    <svg aria-hidden="true" focusable="false" viewBox="0 0 16 16" width="16" height="16">
      <path d="M5 3l8 5-8 5z" fill="currentColor" />
    </svg>
  );
}

/**
 * "Start all" (`services.start` with no targets, i.e. all). Sent through the reconnect queue,
 * so it also survives a daemon that is coming back; disabled with its reason when the
 * connection cannot do it.
 */
export function StartAll({ collapsed }: { collapsed: boolean }) {
  const { t } = useTranslation("shell");
  const errorMessage = useErrorMessage();
  const runtime = useRuntime();
  const capability = useCapability("services.start");
  const reason = useCapabilityReason(capability);
  const [busy, setBusy] = useState(false);
  const disabled = !capability.available;

  const run = () => {
    setBusy(true);
    runtime.queue
      .job("services.start", {})
      .then((handle) => handle.done)
      .catch((error: unknown) => {
        toast.show({ kind: "error", message: errorMessage(error) });
      })
      .finally(() => {
        setBusy(false);
      });
  };

  // Native tooltip on the wrapper: a disabled button receives no pointer events of its own, and
  // Radix stays out of the first paint. The reason is also real text (below) for assistive tech.
  const hint = (enabledText: string) => (disabled && reason !== null ? reason : enabledText);
  const describedBy = disabled ? "start-all-reason" : undefined;
  // The explanation is real text in the DOM (not only a tooltip) so assistive tech reads it.
  const explanation =
    disabled && reason !== null ? (
      <span id="start-all-reason" className="sr-only">
        {reason}
      </span>
    ) : null;

  if (collapsed) {
    return (
      <>
        <span className="mx-auto inline-flex" title={hint(t("sidebar.startAll"))}>
          <IconButton
            label={t("sidebar.startAll")}
            icon={<PlayGlyph />}
            variant="primary"
            disabled={disabled}
            loading={busy}
            aria-describedby={describedBy}
            onClick={run}
          />
        </span>
        {explanation}
      </>
    );
  }
  return (
    <>
      <span className="block [&_button]:w-full" title={hint(t("sidebar.startAllHint"))}>
        <Button
          variant="primary"
          disabled={disabled}
          loading={busy}
          loadingLabel={t("sidebar.startAllBusy")}
          aria-describedby={describedBy}
          onClick={run}
        >
          <PlayGlyph />
          {t("sidebar.startAll")}
        </Button>
      </span>
      {explanation}
    </>
  );
}
