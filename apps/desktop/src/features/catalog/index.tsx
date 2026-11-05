// The Catalog screens (M2-E2-T18, docs/07 section 5.9). The routes import these two.
import { useParams } from "@tanstack/react-router";
import { ToolPage } from "./ToolPage";

export { CatalogPage } from "./CatalogPage";

/** `/catalog/:id`: a tool or a stack. */
export function CatalogToolPage() {
  const id = useParams({
    strict: false,
    select: (params: Record<string, unknown>) => (typeof params.id === "string" ? params.id : ""),
  });
  return <ToolPage key={id} id={id} />;
}
