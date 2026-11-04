import { useTranslation } from "react-i18next";
import { DashboardSectionsHost } from "../../app/hosts";
import { dashboardSections, useSlot } from "../../app/slots";
import { EmptyState } from "../../design/primitives/EmptyState";

/** Id under which M2-E5 registers the real ranking (`registerDashboardSection`). */
export const PROJECTS_SECTION = "projects";

/**
 * Projetos: the section registered under {@link PROJECTS_SECTION}, or an empty state until then
 * (`stats.get` is `not_implemented` before E5: that is "not yet", never an error).
 */
export function ProjectsSection() {
  const { t } = useTranslation("dashboard");
  const registered = useSlot(dashboardSections).some((section) => section.id === PROJECTS_SECTION);
  if (registered) return <DashboardSectionsHost only={PROJECTS_SECTION} />;
  return (
    <section aria-labelledby="dashboard-projects" data-testid="dashboard-projects">
      <h2 id="dashboard-projects" className="pb-2 text-headline">
        {t("projects.title")}
      </h2>
      <div className="rounded-card border border-stroke bg-surface">
        <EmptyState
          title={t("projects.empty.title")}
          description={t("projects.empty.description")}
        />
      </div>
    </section>
  );
}
