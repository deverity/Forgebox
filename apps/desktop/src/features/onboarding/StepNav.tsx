import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";

/** Back / next buttons under a step; `onBack` is `null` on the first step. */
export function StepNav({
  onBack,
  onNext,
  nextDisabled = false,
  nextLabel,
}: {
  onBack: (() => void) | null;
  onNext: () => void;
  nextDisabled?: boolean;
  nextLabel?: ReactNode;
}) {
  const { t } = useTranslation("onboarding");
  return (
    <div className="flex items-center justify-between gap-2 pt-2">
      {onBack === null ? (
        <span />
      ) : (
        <Button variant="ghost" onClick={onBack}>
          {t("nav.back")}
        </Button>
      )}
      <Button variant="primary" disabled={nextDisabled} onClick={onNext}>
        {nextLabel ?? t("nav.next")}
      </Button>
    </div>
  );
}
