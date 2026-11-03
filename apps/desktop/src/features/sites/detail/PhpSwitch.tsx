import { AlertDialog } from "radix-ui";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useCapability } from "../../../state";
import { useCapabilityReason } from "../../../app/shell/useErrorText";
import { Badge } from "../../../design/primitives/Badge";
import { Button } from "../../../design/primitives/Button";
import { CliCommand } from "../../../design/primitives/CliCommand";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Progress } from "../../../design/primitives/Progress";
import { Select } from "../../../design/primitives/Select";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { renderCli } from "../../../ipc/cli";
import type * as T from "../../../ipc/generated";
import { useForgeQuery, useTools } from "../../../state";
import { phpSeries } from "../model";
import { currentSeries, phpOptions, phpSpec, planSize, seriesOf } from "./model";
import { usePhpSwitch, type PhpSwitch } from "./usePhpSwitch";

const CONFIRM_TESTID = "php-install-confirm";

/** Series the registry offers (`tools.info`), best effort: offline leaves the installed ones. */
function useAvailableSeries(): string[] {
  const info = useForgeQuery("tools.info", { tool: "php" });
  return useMemo(
    () => [...new Set((info.data?.available ?? []).map((v) => seriesOf(v.version)))],
    [info.data],
  );
}

export interface PhpSwitchProps {
  site: T.SiteDto;
}

/**
 * PHP dropdown with optimistic live switch (docs/06 section 2.2): the selection moves at
 * once, a badge shows "Trocando para 8.4…" with the job progress, and success is shown only
 * after `job.done` and `site.php_changed`. A failure restores the old selection with the cause.
 */
export function PhpSwitchControl({ site }: PhpSwitchProps) {
  const { t } = useTranslation("sites");
  const sw = usePhpSwitch(site.id);
  const tool = useTools((state) => state.tools.php);
  const available = useAvailableSeries();
  const capability = useCapability("sites.php");
  const reason = useCapabilityReason(capability);
  const current = currentSeries(site.php);
  const options = useMemo(
    () => phpOptions(phpSeries(tool), available, current),
    [tool, available, current],
  );
  const selected = sw.selected(current);
  const { state } = sw;
  const retryTarget = state.phase === "failed" ? state.target : null;

  return (
    <div className="flex flex-col gap-2" data-php-switch={state.phase}>
      <div className="flex flex-wrap items-center gap-3">
        <Select
          label={t("detail.php.label")}
          width="sm"
          disabled={sw.busy || reason !== null}
          {...(selected !== null && { value: selected })}
          options={options.map((option) => ({
            value: option.value,
            label: option.installed
              ? option.value
              : t("detail.php.notInstalled", { series: option.value }),
          }))}
          onValueChange={(value) => {
            if (value !== current) sw.request(value, site.php?.resolved ?? null);
          }}
        />
        <SwitchBadge state={state} />
      </div>
      <CliCommand
        command={
          selected === null ? null : renderCli("sites.php", { site: site.host, php: selected })
        }
      />
      {reason !== null ? <p className="text-caption text-fg-tertiary">{reason}</p> : null}
      {state.phase === "failed" ? (
        <div data-php-switch-failure="">
          <p className="mb-1 text-callout text-fg-secondary">
            {t("detail.php.restored", { version: current ?? "" })}
          </p>
          <ErrorNotice
            error={state.error}
            {...(retryTarget !== null && {
              onRetry: () => {
                sw.dismiss();
                sw.request(retryTarget, site.php?.resolved ?? null);
              },
            })}
          />
          <Button variant="ghost" onClick={sw.dismiss}>
            {t("detail.php.dismiss")}
          </Button>
        </div>
      ) : null}
      <InstallConfirm site={site} sw={sw} />
    </div>
  );
}

function SwitchBadge({ state }: { state: PhpSwitch["state"] }) {
  const { t } = useTranslation("sites");
  if (state.phase === "switching" || state.phase === "installing") {
    const progress = state.progress;
    const pct = progress?.pct ?? null;
    const installing = state.phase === "installing";
    return (
      <div className="flex min-w-0 items-center gap-2" role="status">
        <Badge tone="info">
          {t(installing ? "detail.php.installing" : "detail.php.switching", {
            version: state.target,
          })}
        </Badge>
        <div className="w-40">
          <Progress
            value={pct}
            label={t(installing ? "detail.php.installProgress" : "detail.php.progress", {
              version: state.target,
            })}
          />
        </div>
      </div>
    );
  }
  if (state.phase === "done") {
    return (
      <div role="status">
        <Badge tone="success">{t("detail.php.done", { version: state.target })}</Badge>
      </div>
    );
  }
  return null;
}

function InstallConfirm({ site, sw }: { site: T.SiteDto; sw: PhpSwitch }) {
  const { t } = useTranslation("sites");
  const format = useFormatters();
  const { state } = sw;
  const open = state.phase === "confirm_install";
  const target = open ? state.target : "";
  let size: string;
  if (state.phase !== "confirm_install" || state.plan.status === "loading") {
    size = t("detail.php.confirm.sizeLoading");
  } else if (state.plan.status === "failed") {
    size = t("detail.php.confirm.sizeFailed");
  } else {
    const info = planSize(state.plan.plan);
    size = info.nothing
      ? t("detail.php.confirm.sizeNone")
      : t(info.approximate ? "detail.php.confirm.sizeApprox" : "detail.php.confirm.size", {
          size: format.bytes(info.bytes),
        });
  }
  return (
    <AlertDialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) sw.cancelInstall();
      }}
    >
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
        <AlertDialog.Content
          data-testid={CONFIRM_TESTID}
          className="fixed top-1/2 left-1/2 w-[440px] max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sheet border border-stroke bg-surface-2 p-4 text-fg-primary shadow-popover"
        >
          <AlertDialog.Title className="text-headline">
            {t("detail.php.confirm.title", { version: target })}
          </AlertDialog.Title>
          <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
            {t("detail.php.confirm.description", { version: target, host: site.host })}
          </AlertDialog.Description>
          <p data-testid="php-install-size" className="mt-2 text-callout text-fg-primary">
            {size}
          </p>
          <div className="mt-3">
            <CliCommand
              command={open ? renderCli("tools.install", { specs: [phpSpec(target)] }) : null}
            />
          </div>
          <div className="mt-4 flex justify-end gap-2">
            <AlertDialog.Cancel asChild>
              <Button variant="secondary">{t("detail.php.confirm.cancel")}</Button>
            </AlertDialog.Cancel>
            <Button variant="primary" onClick={sw.confirmInstall}>
              {t("detail.php.confirm.confirm")}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
