import { useState } from "react";
import { useTranslation } from "react-i18next";
import { loose } from "../../looseT";
import { matchesSearch } from "../../model";
import type { SectionProps } from "../../registry";
import { CaCard } from "./CaCard";
import { Certificates } from "./Certificates";
import { HelperLog } from "./HelperLog";

/** Row ids of the section (`security.<id>.label` in the `settings` namespace). */
export const SECURITY_ROWS = ["ca", "certs", "helper"] as const;

/**
 * Seguranca (docs/11 sections 1 and 3): the local CA (status, install, export, rotate,
 * uninstall), the certificates it issued and the log of the elevated helper.
 */
export function SecuritySection({ query }: SectionProps) {
  const { t } = useTranslation("settings");
  // Fixed at mount: the expiry badges do not need to tick while the screen is open.
  const [now] = useState(() => new Date());
  const show = (id: (typeof SECURITY_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`security.${id}.label`));
  return (
    <div className="flex flex-col">
      {show("ca") ? <CaCard /> : null}
      {show("certs") ? <Certificates now={now} /> : null}
      {show("helper") ? <HelperLog /> : null}
    </div>
  );
}
