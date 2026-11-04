import { useTranslation } from "react-i18next";
import { Badge, type BadgeTone } from "../../../../design/primitives/Badge";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import type { PortState } from "../../../../ipc/generated";
import { useForgeQuery } from "../../../../state";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";

const CAPTION_FORGE_SERVICE_PORTS = "forge service ports";
const METHOD_SERVICES_PORTS = "services.ports";
const TID_PORT_EXTERNAL = "port-external";
const TID_PORT_MAP = "port-map";

const TONE: Record<PortState, BadgeTone> = {
  reserved: "neutral",
  bound: "success",
  conflict: "danger",
};

/**
 * Read-only `services.ports`: every port Forgebox reserved (owner, name, state) and, apart,
 * the processes outside Forgebox that sit on a port of its ranges (external conflicts), with
 * pid and executable. Changing a port belongs to the service, not to this screen.
 */
export function PortMap() {
  const { t } = useTranslation("settings");
  const query = useForgeQuery("services.ports", {});
  const map = query.data;
  return (
    <RowFrame
      id="portMap"
      label={t("network.portMap.label")}
      description={t("network.portMap.description")}
      caption={CAPTION_FORGE_SERVICE_PORTS}
    >
      <div className="flex w-full min-w-72 flex-col gap-3">
        {query.error !== null ? (
          <ErrorNotice
            error={query.error}
            context={{ method: METHOD_SERVICES_PORTS }}
            onRetry={() => {
              void query.refetch();
            }}
          />
        ) : null}
        {map === undefined ? null : (
          <>
            {map.ports.length === 0 ? (
              <p className="text-callout text-fg-secondary">{t("network.portMap.empty")}</p>
            ) : (
              <table data-testid={TID_PORT_MAP} className="w-full text-left text-callout">
                <caption className="sr-only">{t("network.portMap.label")}</caption>
                <thead className="text-caption text-fg-tertiary">
                  <tr>
                    <th scope="col" className="pr-3 pb-1 font-normal">
                      {t("network.portMap.port")}
                    </th>
                    <th scope="col" className="pr-3 pb-1 font-normal">
                      {t("network.portMap.owner")}
                    </th>
                    <th scope="col" className="pb-1 font-normal">
                      {t("network.portMap.state")}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {map.ports.map((port) => (
                    <tr key={port.id} data-testid={`port-${String(port.port)}`}>
                      <td className="pr-3 font-mono text-mono tabular-nums">
                        {port.port}/{port.protocol}
                      </td>
                      <td className="pr-3">
                        {port.owner_name} <span className="text-fg-tertiary">{port.name}</span>
                      </td>
                      <td>
                        <Badge tone={TONE[port.state]}>
                          {loose(t)(`network.portMap.states.${port.state}`)}
                        </Badge>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <div data-testid={TID_PORT_EXTERNAL}>
              <h3 className="text-callout font-medium">{t("network.portMap.externalTitle")}</h3>
              {map.external.length === 0 ? (
                <p className="text-callout text-fg-secondary">{t("network.portMap.noExternal")}</p>
              ) : (
                <ul className="flex flex-col gap-1 pt-1">
                  {map.external.map((item) => (
                    <li
                      key={`${String(item.port)}/${item.protocol}`}
                      data-testid={`port-external-${String(item.port)}`}
                      className="text-callout"
                    >
                      <Badge tone="warning">{t("network.portMap.conflict")}</Badge>{" "}
                      <span className="font-mono text-mono tabular-nums">
                        {item.port}/{item.protocol}
                      </span>{" "}
                      {t("network.portMap.heldBy", {
                        exe: item.holder.exe,
                        pid: item.holder.pid,
                      })}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        )}
      </div>
    </RowFrame>
  );
}
