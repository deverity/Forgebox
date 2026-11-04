// The site detail "Banco" tab (M2-E3-T14), registered through the slot registry (docs/07 §4,
// §5.4). Order 50 keeps the doc's sequence: Geral, Desempenho, Processos, Ambiente, Banco...
import { lazy } from "react";
import { registerSiteTab } from "../../app/slots";

const DatabaseTab = lazy(() =>
  import("../sites/detail/tabs/DatabaseTab").then((m) => ({ default: m.DatabaseTab })),
);

registerSiteTab({
  id: "database",
  order: 50,
  label: { ns: "databases", key: "siteTab.label" },
  component: DatabaseTab,
});
