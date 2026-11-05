import { useTranslation } from "react-i18next";
import { Badge } from "../../../../design/primitives/Badge";
import { Toggle } from "../../../../design/primitives/Toggle";
import { loose } from "../../looseT";
import { matchesSearch, valueAt } from "../../model";
import type { SectionProps } from "../../registry";
import { RowFrame } from "../../RowFrame";
import { DnsCard } from "./DnsCard";
import { rangesOf } from "./model";
import { PortMap } from "./PortMap";
import { PortsEditor } from "./PortsEditor";
import { TldRow } from "./TldRow";

const CAPTION_FORGE_LAN_EXPOSE_SITE = "forge lan expose <site>";

/** Row ids of the section (`network.<id>.label` in the `settings` namespace). */
export const NETWORK_ROWS = ["tld", "dns", "ports", "portMap", "lan"] as const;

const DEFAULT_TLD = ".test";

/**
 * Rede (docs/07 section 5.11, docs/06 section 5): TLD (a change rotates the CA), local DNS,
 * port ranges with the live port check, the read-only port map with external conflicts, and
 * the LAN switch that M3-E3 will turn on.
 */
export function NetworkSection({ query, doc }: SectionProps) {
  const { t } = useTranslation("settings");
  const show = (id: (typeof NETWORK_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`network.${id}.label`));
  const tld = valueAt(doc?.values, "tld");
  return (
    <div className="flex flex-col">
      {show("tld") && typeof tld === "string" ? <TldRow value={tld} /> : null}
      {show("tld") && doc !== undefined && typeof tld !== "string" ? (
        <TldRow value={DEFAULT_TLD} />
      ) : null}
      {show("dns") ? <DnsCard /> : null}
      {show("ports") ? <PortsEditor ranges={rangesOf(doc?.values)} /> : null}
      {show("portMap") ? <PortMap /> : null}
      {show("lan") ? (
        <RowFrame
          id="lan"
          label={t("network.lan.label")}
          description={t("network.lan.description")}
          caption={CAPTION_FORGE_LAN_EXPOSE_SITE}
        >
          <Badge>{t("network.lan.soon")}</Badge>
          <Toggle
            label={t("network.lan.label")}
            checked={false}
            disabled
            onCheckedChange={() => undefined}
          />
        </RowFrame>
      ) : null}
    </div>
  );
}
