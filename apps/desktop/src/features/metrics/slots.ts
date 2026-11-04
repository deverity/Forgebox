// The "Desempenho" tab of the site detail (M2-E5-T16), registered through the slot registry so
// no E2 file changes. Order 20 keeps docs/07 section 5.4: Geral, Desempenho, Processos...
// The metrics namespace and the tab (with uPlot, lazy inside it) load only when the site detail
// is opened, never in the initial bundle.
import i18n from "i18next";
import { lazy } from "react";
import { registerSiteTab } from "../../app/slots";

const SitePerformanceTab = lazy(async () => {
  await i18n.loadNamespaces(["metrics", "errors"]);
  const m = await import("./SitePerformanceTab");
  return { default: m.SitePerformanceTab };
});

registerSiteTab({
  id: "performance",
  order: 20,
  label: { ns: "metrics", key: "title" },
  component: SitePerformanceTab,
});
