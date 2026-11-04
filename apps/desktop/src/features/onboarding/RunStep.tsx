import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../actions/registry";
import { addSite } from "../../actions/sites";
import { Badge, type BadgeTone } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { PathField } from "../../design/primitives/PathField";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type { DnsStatusDto, SiteDto } from "../../ipc/generated";
import { onPathsDropped, openUrl } from "../../ipc/native";
import { useHello, useRuntime } from "../../state";
import { InstallProgressList, type InstallProgressItem } from "../shared/install-progress";
import { equivalentCommands, stepCommand } from "./commands";
import {
  DEFAULT_TLD,
  normalizeTld,
  type Choices,
  type RunStep as RunStepState,
  type RunStatus,
} from "./model";
import type { SetupRun } from "./useSetupRun";

const TIP_KEYS = ["t1", "t2", "t3", "t4"] as const;
const TIP_MS = 8000;

const TONE: Record<RunStatus, BadgeTone> = {
  pending: "neutral",
  running: "info",
  done: "success",
  failed: "danger",
  skipped: "warning",
};

function Tips() {
  const { t } = useTranslation("onboarding");
  const [index, setIndex] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => {
      setIndex((current) => (current + 1) % TIP_KEYS.length);
    }, TIP_MS);
    return () => {
      clearInterval(timer);
    };
  }, []);
  const key = TIP_KEYS[index] ?? "t1";
  return (
    <p className="text-callout text-fg-secondary" data-testid="onboarding-tip">
      <span className="text-fg-primary">{t("run.tips.label")}: </span>
      {t(`run.tips.${key}`)}
    </p>
  );
}

function DnsResult({ dns, tld }: { dns: DnsStatusDto; tld: string }) {
  const { t } = useTranslation("onboarding");
  const tlds = dns.tlds.length > 0 ? dns.tlds.join(", ") : tld;
  const mode = dns.mode ?? (dns.policy === "hosts_fallback" ? "hosts" : "nrpt");
  if (mode === "nrpt") return <>{t("run.result.dnsNrpt", { tlds, tld })}</>;
  if (mode === "none") return <>{t("run.result.dnsNone", { tlds })}</>;
  const reason = dns.reason ?? null;
  return (
    <>
      {reason === null
        ? t("run.result.dnsHostsNoReason", { tld })
        : t("run.result.dnsHosts", { tld, reason: t(`run.dnsReason.${reason}`) })}
    </>
  );
}

function StepResult({ step, run, tld }: { step: RunStepState; run: SetupRun; tld: string }) {
  const { t } = useTranslation("onboarding");
  if (step.status !== "done") return null;
  let text: React.ReactNode = null;
  if (step.id === "path" && run.results.path !== undefined) text = t("run.result.path");
  else if (step.id === "dns" && run.results.dns !== undefined) {
    text = <DnsResult dns={run.results.dns} tld={tld} />;
  } else if (step.id === "ca" && run.results.ca !== undefined) {
    text = run.results.ca.trusted_current_user
      ? t("run.result.caTrusted")
      : t("run.result.caNotTrusted");
  }
  if (text === null) return null;
  return (
    <p className="text-callout text-fg-secondary" data-result={step.id}>
      {text}
    </p>
  );
}

function StepRow({
  step,
  run,
  tld,
  onRetryInstall,
}: {
  step: RunStepState;
  run: SetupRun;
  tld: string;
  onRetryInstall: (item: InstallProgressItem) => void;
}) {
  const { t } = useTranslation("onboarding");
  const command = stepCommand(step.id, run.specs);
  const canRetry = step.status === "failed" || step.skip === "elevation";
  const canSkip = step.status === "failed" || step.status === "pending";
  return (
    <li
      data-run-step={step.id}
      data-status={step.status}
      data-skip={step.skip ?? undefined}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 text-body">{t(`run.step.${step.id}`)}</span>
        <Badge tone={TONE[step.status]}>{t(`run.status.${step.status}`)}</Badge>
        {canRetry ? (
          <Button
            variant="ghost"
            onClick={() => {
              run.retry(step.id);
            }}
          >
            {t("run.retry")}
          </Button>
        ) : null}
        {canSkip ? (
          <Button
            variant="ghost"
            onClick={() => {
              run.skip(step.id);
            }}
          >
            {t("run.skip")}
          </Button>
        ) : null}
      </div>
      {step.id === "install" && run.rows.length > 0 ? (
        <InstallProgressList items={run.rows} onRetry={onRetryInstall} />
      ) : null}
      {step.status === "skipped" && step.skip !== null ? (
        <p className="text-callout text-fg-secondary" role="status">
          {t(`run.skipped.${step.skip}`)}
        </p>
      ) : null}
      {step.status === "failed" && step.error !== null ? (
        <ErrorNotice
          error={step.error}
          onRetry={() => {
            run.retry(step.id);
          }}
        />
      ) : null}
      {(step.status === "failed" || step.status === "skipped") &&
      step.skip !== "nothing" &&
      command !== null ? (
        <div className="flex flex-col gap-1">
          <span className="text-caption text-fg-secondary">{t("run.runLater")}</span>
          <CliCommand command={command} />
        </div>
      ) : null}
      <StepResult step={step} run={run} tld={tld} />
    </li>
  );
}

/** "Seu primeiro site": drop a folder (local daemons) or type its path; `sites.add` creates it. */
function FirstSite() {
  const { t } = useTranslation("onboarding");
  const runtime = useRuntime();
  const hello = useHello();
  const remote = hello !== null && !hello.local;
  const [dir, setDir] = useState("");
  const [hover, setHover] = useState(false);
  const [busy, setBusy] = useState(false);
  const [site, setSite] = useState<SiteDto | null>(null);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);

  const add = (path: string) => {
    if (path.trim() === "" || busy) return;
    setBusy(true);
    setFailure(null);
    runAction(runtime.queue, addSite, { dir: path.trim() }, { intent: newIntent() }).then(
      (created) => {
        setBusy(false);
        setSite(created as SiteDto);
      },
      (error: unknown) => {
        setBusy(false);
        setFailure(toForgeApiError(error, { method: addSite.method }));
      },
    );
  };

  const addRef = useRef(add);
  useEffect(() => {
    addRef.current = add;
  });
  useEffect(() => {
    if (remote) return undefined;
    let stop: (() => void) | null = null;
    let alive = true;
    void onPathsDropped((paths) => {
      const first = paths[0];
      if (first !== undefined) {
        setDir(first);
        addRef.current(first);
      }
    }, setHover).then((unsubscribe) => {
      if (alive) stop = unsubscribe;
      else unsubscribe();
    });
    return () => {
      alive = false;
      stop?.();
    };
  }, [remote]);

  return (
    <section
      aria-label={t("run.first.title")}
      data-testid="first-site"
      data-dropping={hover}
      className="flex flex-col gap-3 rounded-card border border-dashed border-stroke-strong bg-surface p-4"
    >
      <h3 className="text-headline">{t("run.first.title")}</h3>
      <p className="text-callout text-fg-secondary">
        {hover ? t("run.first.dropActive") : t("run.first.drop")}
      </p>
      {remote ? (
        <p className="text-caption text-fg-secondary">{t("run.first.remoteHint")}</p>
      ) : null}
      <PathField
        label={t("run.first.pathLabel")}
        width="full"
        value={dir}
        onChange={setDir}
        showDisabledBrowse
      />
      <div>
        <Button
          variant="secondary"
          loading={busy}
          loadingLabel={t("run.first.adding")}
          disabled={dir.trim() === ""}
          onClick={() => {
            add(dir);
          }}
        >
          {t("run.first.add")}
        </Button>
      </div>
      {dir.trim() === "" ? null : <CliCommand command={addSite.cli({ dir: dir.trim() })} />}
      {failure === null ? null : (
        <ErrorNotice
          error={failure}
          onRetry={() => {
            add(dir);
          }}
        />
      )}
      {site === null ? null : (
        <p className="flex items-center gap-2 text-body" role="status" data-testid="site-created">
          {t("run.first.created", { host: site.host })}
          <Button
            variant="ghost"
            onClick={() => {
              void openUrl(site.url);
            }}
          >
            {t("run.first.open")}
          </Button>
        </p>
      )}
    </section>
  );
}

/**
 * Step 4: the run (see `useSetupRun`), the tips while it downloads, the first-site drop zone
 * once it ends and the collapsible list of equivalent commands.
 */
export function RunStepView({
  run,
  choices,
  showRoot,
  finishing,
  onFinish,
  onRetryInstall,
}: {
  run: SetupRun;
  choices: Choices;
  showRoot: boolean;
  finishing: boolean;
  onFinish: () => void;
  onRetryInstall: (item: InstallProgressItem) => void;
}) {
  const { t } = useTranslation("onboarding");
  const tld = normalizeTld(choices.tld) || DEFAULT_TLD;
  const finished = run.phase === "finished";
  const commands = equivalentCommands(choices, {
    showRoot,
    steps: run.steps.map((step) => step.id),
    specs: run.specs,
  });
  return (
    <div className="flex flex-col gap-4">
      <ol aria-label={t("run.stepsLabel")} className="flex flex-col gap-2">
        {run.steps.map((step) => (
          <StepRow key={step.id} step={step} run={run} tld={tld} onRetryInstall={onRetryInstall} />
        ))}
      </ol>
      {finished ? null : <Tips />}
      {finished ? <FirstSite /> : null}
      <details className="rounded-card border border-stroke bg-surface p-3" data-testid="commands">
        <summary className="cursor-pointer text-headline">{t("run.commands.title")}</summary>
        <p className="py-2 text-callout text-fg-secondary">{t("run.commands.description")}</p>
        <ul className="flex flex-col gap-2">
          {commands.map((row) => (
            <li key={`${row.id}-${row.command}`}>
              <CliCommand command={row.command} />
            </li>
          ))}
        </ul>
      </details>
      <div className="flex justify-end">
        <Button
          variant="primary"
          disabled={!finished}
          loading={finishing}
          loadingLabel={t("nav.finishing")}
          onClick={onFinish}
        >
          {t("nav.finish")}
        </Button>
      </div>
    </div>
  );
}
