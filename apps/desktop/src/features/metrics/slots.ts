// Registrations of the metrics feature, all through the slot registry so no E2 file changes:
//  - the site "Desempenho" tab (M2-E5-T16), order 20 as in docs/07 section 5.4;
//  - the Dashboard "Projetos" and machine monitor sections and the performance alerts source of
//    Atenção (M2-E5-T17).
// The tab and the sections are lazy: the metrics namespace and uPlot load only when a site detail
// or the Dashboard sections render, never in the initial bundle. The Atenção source is a hook, so
// it is imported eagerly (the rules are tiny).
import i18n from "i18next";
import { lazy } from "react";
import { registerAttentionSource, registerDashboardSection, registerSiteTab } from "../../app/slots";
import { usePerformanceItems } from "./attention";

const SitePerformanceTab = lazy(async () => {
  await i18n.loadNamespaces(["metrics", "errors"]);
  const m = await import("./SitePerformanceTab");
  return { default: m.SitePerformanceTab };
});
const ProjectsSection = lazy(() =>
  import("./ProjectsSection").then((m) => ({ default: m.ProjectsSection })),
);
const MachineMonitor = lazy(() =>
  import("./MachineMonitor").then((m) => ({ default: m.MachineMonitor })),
);

registerSiteTab({
  id: "performance",
  order: 20,
  label: { ns: "metrics", key: "title" },
  component: SitePerformanceTab,
});
registerDashboardSection({
  id: "projects",
  order: 10,
  label: { ns: "metrics", key: "projects.title" },
  component: ProjectsSection,
});
registerDashboardSection({
  id: "machine",
  order: 20,
  label: { ns: "metrics", key: "machine.title" },
  component: MachineMonitor,
});
// After the Dashboard's own sources (10..70): performance is the last thing to look at.
registerAttentionSource({ id: "performance", order: 80, useItems: usePerformanceItems });
