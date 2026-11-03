// Site detail tabs owned by this feature (M2-E2-T16). Other epics register theirs the same
// way (Desempenho E5, Ambiente/Banco E3, Compartilhar M3); `order` keeps the doc's sequence
// (docs/07 section 5.4): Geral, Desempenho, Processos, Ambiente, Banco, Compartilhar, Diagnóstico.
import { lazy } from "react";
import { registerSiteTab } from "../../app/slots";

const GeneralTab = lazy(() =>
  import("./detail/GeneralTab").then((m) => ({ default: m.GeneralTab })),
);
const ProcessesTab = lazy(() =>
  import("./detail/ProcessesTab").then((m) => ({ default: m.ProcessesTab })),
);
const DiagnosticsTab = lazy(() =>
  import("./detail/DiagnosticsTab").then((m) => ({ default: m.DiagnosticsTab })),
);

registerSiteTab({
  id: "general",
  order: 10,
  label: { ns: "sites", key: "detail.tabs.general" },
  component: GeneralTab,
});
registerSiteTab({
  id: "processes",
  order: 30,
  label: { ns: "sites", key: "detail.tabs.processes" },
  component: ProcessesTab,
});
registerSiteTab({
  id: "diagnostics",
  order: 90,
  label: { ns: "sites", key: "detail.tabs.diagnostics" },
  component: DiagnosticsTab,
});
