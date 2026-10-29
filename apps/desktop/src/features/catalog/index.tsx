import { useParams } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { PlaceholderPage } from "../../app/shell/PlaceholderPage";

/** Placeholder until M2-E2-T18. */
export function CatalogPage() {
  return <PlaceholderPage id="catalog" />;
}

/** Placeholder for `/catalog/:id` until M2-E2-T18. */
export function CatalogToolPage() {
  const { t } = useTranslation("shell");
  const id = useParams({
    strict: false,
    select: (params: Record<string, unknown>) => (typeof params.id === "string" ? params.id : ""),
  });
  return (
    <PlaceholderPage id="catalog">
      <h2 className="px-4 text-headline">{t("placeholder.toolId", { id: id })}</h2>
    </PlaceholderPage>
  );
}
