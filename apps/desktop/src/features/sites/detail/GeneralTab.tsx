import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { setSiteDriver, setSiteSecure, setSiteServer } from "../../../actions/sites";
import { Badge, type BadgeTone } from "../../../design/primitives/Badge";
import { Button } from "../../../design/primitives/Button";
import { ActionButton } from "../../../design/primitives/ActionButton";
import { CliCommand } from "../../../design/primitives/CliCommand";
import { Select } from "../../../design/primitives/Select";
import { TextField } from "../../../design/primitives/TextField";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { renderCli } from "../../../ipc/cli";
import { copyText } from "../../../ipc/native";
import type * as T from "../../../ipc/generated";
import { useForgeQuery, useSite, useTools } from "../../../state";
import { daysUntil } from "./model";
import { PhpSwitchControl } from "./PhpSwitch";

const EXPIRING_DAYS = 30;

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[140px_1fr] items-start gap-3 py-2">
      <dt className="text-callout text-fg-secondary">{label}</dt>
      <dd className="min-w-0 text-body text-fg-primary">{children}</dd>
    </div>
  );
}

/** HTTPS state with the certificate of the host (`tls.list`: validity, trust). */
function CertificateStatus({ site }: { site: T.SiteDto }) {
  const { t } = useTranslation("sites");
  const format = useFormatters();
  const [now] = useState(() => Date.now());
  const certs = useForgeQuery("tls.list", {}, { enabled: site.secure });
  if (!site.secure) return <Badge>{t("detail.general.httpsOff")}</Badge>;
  let tone: BadgeTone = "neutral";
  let text: string;
  const cert = certs.data?.certificates.find((c) => c.host === site.host);
  if (certs.data === undefined) {
    text = t("detail.general.cert.loading");
  } else if (cert === undefined) {
    text = t("detail.general.cert.none");
    tone = "warning";
  } else {
    const days = daysUntil(cert.not_after, now);
    if (days < 0) {
      text = t("detail.general.cert.expired");
      tone = "danger";
    } else if (cert.trusted === false) {
      text = t("detail.general.cert.untrusted");
      tone = "warning";
    } else if (days < EXPIRING_DAYS) {
      text = t("detail.general.cert.expiring", { days });
      tone = "warning";
    } else {
      text = t("detail.general.cert.valid", { date: format.date(Date.parse(cert.not_after)) });
      tone = "success";
    }
  }
  return (
    <span className="flex flex-wrap items-center gap-2">
      <Badge tone="accent">{t("detail.general.httpsOn")}</Badge>
      <span data-testid="cert-status" data-tone={tone}>
        <Badge tone={tone}>{text}</Badge>
      </span>
    </span>
  );
}

function CopyRoot({ root }: { root: string }) {
  const { t } = useTranslation("sites");
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <Button
      variant="ghost"
      onClick={() => {
        void copyText(root).then((ok) => {
          setState(ok ? "copied" : "failed");
        });
      }}
    >
      {state === "copied"
        ? t("detail.general.copied")
        : state === "failed"
          ? t("detail.general.copyFailed")
          : t("detail.general.copyRoot")}
    </Button>
  );
}

/** Web server of the site: the installed web servers, applied with `sites.server`. */
function ServerRow({ site }: { site: T.SiteDto }) {
  const { t } = useTranslation("sites");
  const tools = useTools((state) => state.tools);
  const [draft, setDraft] = useState<string | null>(null);
  const options = Object.values(tools)
    .filter((tool) => tool.category === "webserver" && tool.installed.length > 0)
    .map((tool) => ({ value: tool.id, label: tool.name }));
  const current = site.server ?? "";
  if (current !== "" && !options.some((option) => option.value === current)) {
    options.push({ value: current, label: current });
  }
  const chosen = draft ?? current;
  if (options.length === 0) return <span>{current}</span>;
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Select
        label={t("detail.general.serverSelect")}
        value={chosen}
        options={options}
        onValueChange={setDraft}
        width="sm"
      />
      <ActionButton
        action={setSiteServer}
        input={{ site: site.id, server: chosen }}
        loadingLabel={t("detail.general.serverApplying")}
        {...((chosen === current || chosen === "") && {
          disabledReason: t("detail.general.serverSame"),
        })}
        onDone={() => {
          setDraft(null);
        }}
      >
        {t("detail.general.serverApply")}
      </ActionButton>
    </div>
  );
}

function DriverRow({ site }: { site: T.SiteDto }) {
  const { t } = useTranslation("sites");
  const [draft, setDraft] = useState("");
  const editable = site.kind === "php" || site.kind === "static";
  return (
    <div className="flex flex-col gap-2">
      <span className="font-mono text-mono">{site.driver ?? t("detail.general.driverNone")}</span>
      {editable ? (
        <div className="flex flex-wrap items-end gap-2">
          <TextField
            label={t("detail.general.driverInput")}
            width="sm"
            value={draft}
            onChange={(event) => {
              setDraft(event.target.value);
            }}
          />
          <ActionButton
            action={setSiteDriver}
            input={{ site: site.id, driver: draft.trim() }}
            loadingLabel={t("detail.general.driverApplying")}
            {...(draft.trim() === "" && { disabledReason: t("detail.general.driverInput") })}
            onDone={() => {
              setDraft("");
            }}
          >
            {t("detail.general.driverApply")}
          </ActionButton>
          <ActionButton
            action={setSiteDriver}
            input={{ site: site.id, driver: null }}
            loadingLabel={t("detail.general.driverApplying")}
          >
            {t("detail.general.driverAuto")}
          </ActionButton>
        </div>
      ) : null}
    </div>
  );
}

/** "Geral": host, aliases, folder, driver, server, HTTPS and the live PHP switch. */
export function GeneralTab({ siteId }: { siteId: string }) {
  const { t } = useTranslation("sites");
  const site = useSite(siteId);
  if (site === undefined) return null;
  return (
    <div data-testid="site-general" className="flex max-w-3xl flex-col gap-4">
      <dl className="divide-y divide-stroke">
        <Row label={t("detail.general.host")}>
          <span className="font-mono text-mono">{site.host}</span>
        </Row>
        <Row label={t("detail.general.aliases")}>
          {site.aliases.length === 0 ? (
            <span className="text-fg-secondary">{t("detail.general.noAliases")}</span>
          ) : (
            <span className="flex flex-wrap gap-1">
              {site.aliases.map((alias) => (
                <Badge key={alias}>{alias}</Badge>
              ))}
            </span>
          )}
        </Row>
        <Row
          label={site.kind === "proxy" ? t("detail.general.upstream") : t("detail.general.root")}
        >
          {site.kind === "proxy" ? (
            <span className="font-mono text-mono">{site.upstream ?? ""}</span>
          ) : site.root === null ? (
            <span className="text-fg-secondary">{t("detail.general.noRoot")}</span>
          ) : (
            <span className="flex items-center gap-2">
              <span className="min-w-0 truncate font-mono text-mono" title={site.root}>
                {site.root}
              </span>
              <CopyRoot root={site.root} />
            </span>
          )}
        </Row>
        {site.kind === "proxy" ? null : (
          <Row label={t("detail.general.case")}>
            <span data-testid="site-case">
              {site.case_sensitive
                ? t("badge.caseSensitiveTitle")
                : t("badge.caseInsensitiveTitle")}
            </span>
          </Row>
        )}
        <Row label={t("detail.general.driver")}>
          <DriverRow site={site} />
        </Row>
        <Row label={t("detail.general.server")}>
          <ServerRow site={site} />
        </Row>
        <Row label={t("detail.general.https")}>
          <div className="flex flex-wrap items-center gap-3">
            <CertificateStatus site={site} />
            <ActionButton
              action={setSiteSecure}
              input={{ site: site.id, secure: !site.secure }}
              loadingLabel={t("detail.general.httpsApplying")}
            >
              {site.secure ? t("detail.general.httpsDisable") : t("detail.general.httpsEnable")}
            </ActionButton>
          </div>
        </Row>
        <Row label={t("detail.general.php")}>
          {site.kind === "php" && site.php !== null ? (
            <PhpSwitchControl site={site} />
          ) : (
            <span className="text-fg-secondary">{t("detail.general.phpNone")}</span>
          )}
        </Row>
      </dl>
      <CliCommand command={renderCli("sites.info", { site: site.host })} />
    </div>
  );
}
