import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { Action } from "../../../../actions/registry";
import { useCapabilityReason } from "../../../../app/shell/useErrorText";
import { Button, type ButtonVariant } from "../../../../design/primitives/Button";
import { Tooltip } from "../../../../design/primitives/Tooltip";
import type { MethodName } from "../../../../ipc/methods";
import { useCapability } from "../../../../state";
import type { JobRun } from "./useJobAction";

export interface JobButtonProps<M extends MethodName, I> {
  action: Action<M, I>;
  input: I;
  job: JobRun<I>;
  variant?: ButtonVariant;
  children: ReactNode;
  /** Overrides what the click does (open a confirmation instead of running at once). */
  onPress?: () => void;
  /** Extra reason that disables the button. */
  disabledReason?: string | undefined;
  testId?: string;
}

/**
 * Button of a job action run through {@link useJobAction}: disabled with a visible reason when
 * the capabilities refuse the method (role, not implemented), busy while the job runs, and its
 * tooltip carries the equivalent `forge` command (also in `data-forge-cli`).
 */
export function JobButton<M extends MethodName, I>({
  action,
  input,
  job,
  variant = "secondary",
  children,
  onPress,
  disabledReason,
  testId,
}: JobButtonProps<M, I>) {
  const { t } = useTranslation("settings");
  const capability = useCapability(action.method);
  const capabilityReason = useCapabilityReason(capability);
  const reason = disabledReason ?? capabilityReason ?? undefined;
  const running = job.phase === "running";
  let cli: string | null = null;
  try {
    cli = action.cli(input);
  } catch {
    cli = null;
  }
  return (
    <Tooltip content={reason ?? children} {...(cli === null ? {} : { command: cli })}>
      <span
        data-action={action.id}
        data-forge-cli={cli ?? undefined}
        data-disabled-reason={reason}
        className="inline-flex"
      >
        <Button
          variant={variant}
          loading={running}
          loadingLabel={t("job.running")}
          disabled={reason !== undefined}
          {...(testId === undefined ? {} : { "data-testid": testId })}
          onClick={() => {
            if (onPress !== undefined) onPress();
            else job.run(input);
          }}
        >
          {children}
        </Button>
      </span>
    </Tooltip>
  );
}
