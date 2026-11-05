import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { EmptyState } from "../../design/primitives/EmptyState";
import { useConnectionStatus } from "../../state";
import { featureOf, type FeatureId } from "../features";

/**
 * What a route shows until its task lands: the screen's name and the roadmap task that
 * delivers it. The real screen replaces the page component in `src/features/<id>/index.tsx`.
 * It renders with the daemon down too (every route stays navigable).
 */
export function PlaceholderPage({ id, children }: { id: FeatureId; children?: ReactNode }) {
  const { t } = useTranslation("shell");
  const connection = useConnectionStatus();
  const feature = featureOf(id);
  return (
    <div data-testid={`page-${id}`} className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <h1 className="px-4 pt-4 text-title-1">{t(`nav.${id}`)}</h1>
      <EmptyState
        title={t("placeholder.title", { feature: t(`nav.${id}`) })}
        description={
          connection === "connected"
            ? t("placeholder.description", { task: feature.task })
            : `${t("placeholder.description", { task: feature.task })} ${t("placeholder.unavailable")}`
        }
      />
      {children}
    </div>
  );
}
