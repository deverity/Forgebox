import type { UseQueryResult } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Skeleton } from "../../../design/primitives/Skeleton";
import type { ForgeApiError } from "../../../ipc/errors";

export interface QueryBoundaryProps<T> {
  query: UseQueryResult<T, ForgeApiError>;
  /** Fixed height of the loading placeholder, px. */
  height?: number;
  children: (data: T) => ReactNode;
}

/** Loading skeleton, then the localized failure with Retry, then the data. */
export function QueryBoundary<T>({ query, height = 120, children }: QueryBoundaryProps<T>) {
  const { t } = useTranslation("services");
  if (query.isError) {
    return (
      <ErrorNotice
        error={query.error}
        onRetry={() => {
          void query.refetch();
        }}
      />
    );
  }
  if (query.data === undefined) {
    return (
      <div data-testid="tab-loading" aria-busy="true">
        <span className="sr-only">{t("tab.loading")}</span>
        <Skeleton height={height} />
      </div>
    );
  }
  return <>{children(query.data)}</>;
}
