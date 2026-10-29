import { useParams } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { SiteTabsHost } from "../../app/hosts";
import { PlaceholderPage } from "../../app/shell/PlaceholderPage";

/** Placeholder until M2-E2-T15. */
export function SitesPage() {
  return <PlaceholderPage id="sites" />;
}

/** Placeholder until M2-E2-T16; already hosts the tabs other epics register. */
export function SiteDetailPage() {
  const { t } = useTranslation("shell");
  const id = useParams({
    strict: false,
    select: (params: Record<string, unknown>) => (typeof params.id === "string" ? params.id : ""),
  });
  return (
    <PlaceholderPage id="sites">
      <h2 className="px-4 text-headline">{t("placeholder.siteId", { id: id })}</h2>
      <SiteTabsHost siteId={id} />
    </PlaceholderPage>
  );
}
