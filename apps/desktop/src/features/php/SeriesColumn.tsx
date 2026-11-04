import { Link } from "@tanstack/react-router";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../actions/registry";
import { setCliVersion } from "../../actions/tools";
import { Badge } from "../../design/primitives/Badge";
import { Card } from "../../design/primitives/Card";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { FOCUS_RING } from "../../design/primitives/shared";
import { Tooltip } from "../../design/primitives/Tooltip";
import { TNUM_CLASS } from "../../i18n/format";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type { PhpVersionDto } from "../../ipc/generated";
import { useCapability, useForgeQuery, useRuntime, useTools } from "../../state";
import { QuickSettingsForm } from "../shared/quick-settings";
import { QueryBoundary } from "../services/tabs/QueryBoundary";
import { ExtensionsSection } from "./ExtensionsSection";
import { PHP_TOOL_ID, isCliVersion, seriesTarget, variantBadges } from "./model";
import { PoolCard } from "./PoolCard";
import { UpdateSection } from "./UpdateSection";
import { XdebugSection } from "./XdebugSection";

const SITES_PATH = "/sites";
const SITES_LINK_ID = "series-sites";

/** The "CLI padrão" radio: `tools.use`, and it follows `tool.current_changed` through the store. */
function CliRadio({ version, checked }: { version: PhpVersionDto; checked: boolean }) {
  const { t } = useTranslation("php");
  const runtime = useRuntime();
  const capability = useCapability(setCliVersion.method);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const id = useId();
  const input = { tool: PHP_TOOL_ID, version: version.series };
  return (
    <div className="flex flex-col gap-1">
      <Tooltip content={t("cli.hint")} command={setCliVersion.cli(input) ?? ""}>
        <label
          htmlFor={id}
          data-forge-cli={setCliVersion.cli(input) ?? undefined}
          className="inline-flex items-center gap-2 text-body"
        >
          <input
            id={id}
            type="radio"
            name="php-cli-default"
            value={version.series}
            checked={checked}
            disabled={busy || !capability.available}
            onChange={() => {
              setBusy(true);
              setFailure(null);
              runAction(runtime.queue, setCliVersion, input, { intent: newIntent() }).then(
                () => {
                  setBusy(false);
                },
                (error: unknown) => {
                  setBusy(false);
                  setFailure(toForgeApiError(error, { method: setCliVersion.method }));
                },
              );
            }}
            className={`size-4 accent-(--accent) ${FOCUS_RING}`}
          />
          {busy ? t("cli.switching") : t("cli.label")}
        </label>
      </Tooltip>
      {failure === null ? null : <ErrorNotice error={failure} />}
    </div>
  );
}

function QuickSettings({ series }: { series: string }) {
  const { t } = useTranslation("php");
  const settings = useForgeQuery("services.settings", { target: seriesTarget(series) });
  return (
    <section aria-label={t("quick.title")} className="flex flex-col gap-2">
      <h3 className="text-headline">{t("quick.title")}</h3>
      <QueryBoundary query={settings} height={160}>
        {(dto) => (
          <QuickSettingsForm
            key={JSON.stringify([dto.settings, dto.ports])}
            target={dto.target}
            settings={dto.settings}
            ports={dto.ports ?? []}
          />
        )}
      </QueryBoundary>
    </section>
  );
}

/** One installed series: identity, pool, CLI default, settings, extensions, Xdebug, updates. */
export function SeriesColumn({ version }: { version: PhpVersionDto }) {
  const { t } = useTranslation("php");
  const tool = useTools((state) => state.tools[PHP_TOOL_ID]);
  const cli = isCliVersion(version, tool);
  return (
    <article
      data-php-series={version.series}
      data-cli={cli ? "true" : undefined}
      aria-label={t("column", { series: version.series })}
      className="flex min-h-0 w-80 shrink-0 flex-col gap-4 overflow-y-auto pr-1"
    >
      <Card className="flex flex-col gap-3">
        <header className="flex flex-col gap-1">
          <h2 className="text-title-2">{t("column", { series: version.series })}</h2>
          <div className="flex flex-wrap items-center gap-2">
            <span data-testid="exact-version" className={`text-body ${TNUM_CLASS}`}>
              {version.version}
            </span>
            {variantBadges(version).map((badge) => (
              <Badge key={badge}>{badge}</Badge>
            ))}
          </div>
        </header>
        <PoolCard version={version} />
        <Link
          to={SITES_PATH}
          search={{ php: version.series }}
          data-testid={SITES_LINK_ID}
          aria-label={t("sites.link", { series: version.series })}
          className={`w-fit rounded-control text-callout text-accent underline-offset-2 hover:underline ${FOCUS_RING}`}
        >
          {t("sites.count", { count: version.sites })}
        </Link>
        <CliRadio version={version} checked={cli} />
      </Card>
      <Card className="flex flex-col gap-3">
        <QuickSettings series={version.series} />
      </Card>
      <Card>
        <ExtensionsSection series={version.series} />
      </Card>
      <Card>
        <XdebugSection version={version} />
      </Card>
      <Card>
        <UpdateSection series={version.series} />
      </Card>
    </article>
  );
}
