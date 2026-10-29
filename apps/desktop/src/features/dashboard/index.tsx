import { DashboardSectionsHost } from "../../app/hosts";
import { PlaceholderPage } from "../../app/shell/PlaceholderPage";

/** Placeholder until M2-E2-T14; already hosts the sections other epics register. */
export function DashboardPage() {
  return (
    <PlaceholderPage id="dashboard">
      <DashboardSectionsHost />
    </PlaceholderPage>
  );
}
