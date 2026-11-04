import { useState } from "react";
import { useTranslation } from "react-i18next";
import { rotateCa } from "../../../../actions/system";
import { ConfirmDialog } from "./ConfirmDialog";
import { JobButton } from "./JobButton";
import { JobStatus } from "./JobStatus";
import { useJobAction } from "./useJobAction";

const METHOD_TLS_CA_ROTATE = "tls.ca_rotate";
const TID_ROTATE_DIALOG = "rotate-dialog";

/**
 * "Rotacionar CA" (`forge tls ca rotate`): destructive, so the button opens a modal that says
 * every certificate is reissued; confirming runs the job (`confirm: true` is sent by the
 * action) and the card follows it, with the UAC notice because the new CA is trusted again.
 */
export function RotateCa({ testId }: { testId: string }) {
  const { t } = useTranslation("settings");
  const job = useJobAction(rotateCa);
  const [confirming, setConfirming] = useState(false);
  return (
    <div className="flex flex-col gap-2">
      <div>
        <JobButton
          action={rotateCa}
          input={undefined}
          job={job}
          variant="destructive"
          testId={testId}
          onPress={() => {
            setConfirming(true);
          }}
        >
          {t("security.ca.rotate")}
        </JobButton>
      </div>
      <JobStatus
        job={job}
        label={t("security.ca.rotating")}
        method={METHOD_TLS_CA_ROTATE}
        testId={`${testId}-status`}
        elevated
      />
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={t("security.ca.rotateTitle")}
        description={t("security.ca.rotateBody")}
        confirmLabel={t("security.ca.rotateConfirm")}
        destructive
        command={rotateCa.cli(undefined)}
        testId={TID_ROTATE_DIALOG}
        onConfirm={() => {
          setConfirming(false);
          job.run(undefined);
        }}
      />
    </div>
  );
}
