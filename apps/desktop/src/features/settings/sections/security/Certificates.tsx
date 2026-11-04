import { useTranslation } from "react-i18next";
import { renewCertificate } from "../../../../actions/security";
import { Badge, type BadgeTone } from "../../../../design/primitives/Badge";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import type { CertificateDto } from "../../../../ipc/generated";
import { useForgeQuery } from "../../../../state";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { useJobAction } from "../common/useJobAction";
import { certState, daysLeft, type CertState } from "./model";

const CAPTION_FORGE_TLS_LIST = "forge tls list";
const METHOD_TLS_LIST = "tls.list";
const METHOD_TLS_RENEW = "tls.renew";
const TID_CERTS_EMPTY = "certs-empty";
const TID_CERTS_LIST = "certs-list";
const TID_CERT_RENEW_ALL = "cert-renew-all";
const TID_CERT_RENEW_ALL_STATUS = "cert-renew-all-status";

const TONE: Record<CertState, BadgeTone> = {
  ok: "success",
  expiring: "warning",
  untrusted: "warning",
  expired: "danger",
};

function CertRow({ cert, now }: { cert: CertificateDto; now: Date }) {
  const { t } = useTranslation("settings");
  const renew = useJobAction(renewCertificate);
  const state = certState(cert, now);
  const left = daysLeft(cert.not_after, now);
  return (
    <li data-testid={`cert-${cert.host}`} data-state={state} className="flex flex-col gap-2 py-2">
      <div className="flex flex-wrap items-center gap-3">
        <span className="min-w-40 flex-1 font-mono text-mono [overflow-wrap:anywhere]">
          {cert.host}
        </span>
        <span className="text-callout tabular-nums text-fg-secondary">
          {cert.not_after.slice(0, 10)}
          {state === "expired"
            ? ""
            : ` · ${t("security.certs.daysLeft", { count: Math.max(left, 0) })}`}
        </span>
        <Badge tone={TONE[state]}>{loose(t)(`security.certs.states.${state}`)}</Badge>
        <JobButton
          action={renewCertificate}
          input={{ host: cert.host }}
          job={renew}
          testId={`cert-renew-${cert.host}`}
        >
          {t("security.certs.renew")}
        </JobButton>
      </div>
      <JobStatus
        job={renew}
        label={t("security.certs.renewing", { host: cert.host })}
        method={METHOD_TLS_RENEW}
        testId={`cert-status-${cert.host}`}
      />
    </li>
  );
}

/**
 * Leaf certificates of `certs/` (`tls.list`, soonest expiry first): expiry, whether browsers
 * here would trust each one, and "Renovar" per host or for all (`forge tls renew`).
 */
export function Certificates({ now }: { now: Date }) {
  const { t } = useTranslation("settings");
  const list = useForgeQuery("tls.list", {});
  const renewAll = useJobAction(renewCertificate);
  const certs = list.data?.certificates ?? [];
  return (
    <RowFrame
      id="certificates"
      label={t("security.certs.label")}
      description={t("security.certs.description")}
      caption={CAPTION_FORGE_TLS_LIST}
    >
      <div className="flex w-full min-w-72 flex-col gap-2">
        {list.error !== null ? (
          <ErrorNotice
            error={list.error}
            context={{ method: METHOD_TLS_LIST }}
            onRetry={() => {
              void list.refetch();
            }}
          />
        ) : null}
        {list.data !== undefined && certs.length === 0 ? (
          <p data-testid={TID_CERTS_EMPTY} className="text-callout text-fg-secondary">
            {t("security.certs.empty")}
          </p>
        ) : null}
        {certs.length === 0 ? null : (
          <>
            <ul data-testid={TID_CERTS_LIST} className="flex flex-col divide-y divide-stroke">
              {certs.map((cert) => (
                <CertRow key={cert.host} cert={cert} now={now} />
              ))}
            </ul>
            <div className="flex flex-wrap items-center gap-2">
              <JobButton
                action={renewCertificate}
                input={{ all: true }}
                job={renewAll}
                testId={TID_CERT_RENEW_ALL}
              >
                {t("security.certs.renewAll")}
              </JobButton>
            </div>
            <CliCommand command={renewCertificate.cli({ all: true })} />
            <JobStatus
              job={renewAll}
              label={t("security.certs.renewingAll")}
              method={METHOD_TLS_RENEW}
              testId={TID_CERT_RENEW_ALL_STATUS}
            />
          </>
        )}
      </div>
    </RowFrame>
  );
}
