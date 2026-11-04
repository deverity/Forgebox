import { useRouter } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Skeleton } from "../../design/primitives/Skeleton";
import { useConnectionStatus, useForgeApi } from "../../state";
import { OptionsStep } from "./OptionsStep";
import { RootStep } from "./RootStep";
import { RunStepView } from "./RunStep";
import { StackStep } from "./StackStep";
import { runPlan, type StepId } from "./model";
import { useSetupRun } from "./useSetupRun";
import { useWizard } from "./useWizard";

const HOME = "/dashboard";

function StepHeader({ steps, step }: { steps: readonly StepId[]; step: StepId }) {
  const { t } = useTranslation("onboarding");
  const index = Math.max(0, steps.indexOf(step));
  return (
    <nav aria-label={t("progress.label")}>
      <ol className="flex flex-wrap items-center gap-x-4 gap-y-1">
        {steps.map((id, i) => (
          <li
            key={id}
            data-step={id}
            aria-current={id === step ? "step" : undefined}
            className={
              id === step ? "text-callout text-fg-primary" : "text-callout text-fg-tertiary"
            }
          >
            {`${String(i + 1)}. ${t(`steps.${id}`)}`}
          </li>
        ))}
      </ol>
      <p className="sr-only" aria-live="polite">
        {t("progress.stepOf", { current: index + 1, total: steps.length })}
      </p>
    </nav>
  );
}

/**
 * The first-run wizard (docs/07 section 5.1): root and TLD, stack, permissions, install. Every
 * choice is stored as it is made, so closing the window and opening it again resumes. The page
 * keeps the install running in this component, so the step views only render it.
 */
export function OnboardingPage() {
  const { t } = useTranslation("onboarding");
  const router = useRouter();
  const api = useForgeApi();
  const status = useConnectionStatus();
  const wizard = useWizard();
  const { choices, step, steps } = wizard;
  const [finishing, setFinishing] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const shown = useRef(false);

  const run = useSetupRun({
    stack: choices.stack,
    versions: choices.versions,
    parkedDir: choices.parkedDir,
    steps: runPlan(choices, {
      pathAvailable: wizard.pathAvailable,
      autostartAvailable: wizard.autostart !== null,
    }),
  });

  // Without step 1 nobody started the daemon of an uninitialized root (portable, FORGEBOX_ROOT).
  useEffect(() => {
    if (wizard.ready && wizard.local && !wizard.showRoot && status === "uninitialized") {
      void api.start({ initialize: true }).catch(() => undefined);
    }
  }, [wizard.ready, wizard.local, wizard.showRoot, status, api]);

  // Entering step 4 starts the run, also when the window was closed there and opened again:
  // the plan is made again and only what is still missing is installed.
  const { phase, start } = run;
  useEffect(() => {
    if (wizard.ready && step === "run" && phase === "idle") start();
  }, [wizard.ready, step, phase, start]);

  useEffect(() => {
    if (!shown.current) {
      shown.current = true;
      return;
    }
    heading.current?.focus();
  }, [step]);

  const finish = () => {
    setFinishing(true);
    wizard.finish().then(
      () => {
        void router.navigate({ to: HOME });
      },
      () => {
        setFinishing(false);
      },
    );
  };

  const index = steps.indexOf(step);
  const previous = index > 0 ? steps[index - 1] : undefined;
  const next = steps[index + 1];
  const back =
    previous === undefined
      ? null
      : () => {
          wizard.go(previous);
        };
  const forward = () => {
    if (next !== undefined) wizard.go(next);
  };

  return (
    <div data-testid="page-onboarding" className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 px-6 py-8">
        <header className="flex flex-col gap-2">
          <h1 className="text-title-1">{t("title")}</h1>
          <p className="text-callout text-fg-secondary">{t("subtitle")}</p>
          {wizard.ready ? <StepHeader steps={steps} step={step} /> : null}
          {wizard.ready && wizard.hiddenReason !== null ? (
            <p
              role="note"
              data-testid="onboarding-hidden-note"
              data-reason={wizard.hiddenReason}
              className="rounded-card border border-stroke bg-surface p-3 text-callout text-fg-secondary"
            >
              {t(`hidden.${wizard.hiddenReason}`)}
            </p>
          ) : null}
        </header>

        {wizard.loadError !== null ? (
          <div className="flex flex-col gap-2">
            <p className="text-body">{t("loadFailed")}</p>
            <ErrorNotice error={wizard.loadError} />
          </div>
        ) : !wizard.ready ? (
          <div aria-busy="true">
            <span className="sr-only">{t("loading")}</span>
            <Skeleton height={160} />
          </div>
        ) : (
          <section aria-labelledby="onboarding-step-title" className="flex flex-col gap-4">
            <h2
              id="onboarding-step-title"
              ref={heading}
              tabIndex={-1}
              className="text-title-2 outline-hidden"
            >
              {t(`${step}.title`)}
            </h2>
            <p className="-mt-2 text-callout text-fg-secondary">{t(`${step}.description`)}</p>
            {step === "root" ? <RootStep wizard={wizard} onDone={forward} /> : null}
            {step === "stack" ? <StackStep wizard={wizard} onBack={back} onNext={forward} /> : null}
            {step === "options" ? (
              <OptionsStep
                wizard={wizard}
                onBack={() => {
                  wizard.go("stack");
                }}
                onNext={forward}
              />
            ) : null}
            {step === "run" ? (
              <RunStepView
                run={run}
                choices={choices}
                showRoot={wizard.showRoot}
                finishing={finishing}
                onFinish={finish}
                onRetryInstall={() => {
                  run.retry("install");
                }}
              />
            ) : null}
          </section>
        )}
      </div>
    </div>
  );
}
