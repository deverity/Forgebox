import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Card,
  EmptyState,
  ListRow,
  Menu,
  Popover,
  Progress,
  Sheet,
  Skeleton,
  SkeletonSlot,
  Sparkline,
  toast,
  ToastViewport,
  Tooltip,
  VirtualList,
} from "../primitives";

const ID = { restart: "restart", sep: "sep", remove: "remove" } as const;

const BUTTON =
  "rounded-control border border-stroke bg-surface px-3 py-1 text-body text-fg-primary hover:bg-surface-2 focus-visible:outline-2 focus-visible:outline-accent";

/** Gallery section for the surfaces/feedback primitives (M2-E1-T12); /__gallery mounts it (T11). */
export function SurfacesGallery() {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const rows = useMemo(() => Array.from({ length: 500 }, (_, i) => i + 1), []);
  const samples = useMemo(
    () => Array.from({ length: 60 }, (_, i) => 40 + 30 * Math.sin(i / 6) + (i % 7)),
    [],
  );

  return (
    <section aria-labelledby="gallery-surfaces" className="flex flex-col gap-6 p-6">
      <h2 id="gallery-surfaces" className="text-title-2">
        {t("gallery.surfacesTitle")}
      </h2>

      <Card>
        <h3 className="text-headline">{t("gallery.card")}</h3>
        <p className="text-callout text-fg-secondary">{t("gallery.cardBody")}</p>
      </Card>

      <div className="flex flex-wrap items-center gap-3">
        <Sheet
          trigger={
            <button type="button" className={BUTTON}>
              {t("gallery.sheetOpen")}
            </button>
          }
          title={t("gallery.sheetTitle")}
        >
          <Progress value={64} label={t("gallery.progress")} speedBps={8_400_000} etaSeconds={42} />
        </Sheet>
        <Popover
          label={t("gallery.popoverLabel")}
          trigger={
            <button type="button" className={BUTTON}>
              {t("gallery.popoverOpen")}
            </button>
          }
        >
          {t("gallery.popoverBody")}
        </Popover>
        <Tooltip content={t("gallery.tooltipContent")} command="forge restart nginx">
          <button type="button" className={BUTTON}>
            {t("gallery.tooltipTrigger")}
          </button>
        </Tooltip>
        <button
          type="button"
          className={BUTTON}
          onClick={() => toast.show({ message: t("gallery.toastMessage") })}
        >
          {t("gallery.toastInfo")}
        </button>
        <button
          type="button"
          className={BUTTON}
          onClick={() =>
            toast.show({
              kind: "success",
              message: t("gallery.toastMessage"),
              undo: { onUndo: () => undefined },
            })
          }
        >
          {t("gallery.toastUndo")}
        </button>
        <button
          type="button"
          className={BUTTON}
          onClick={() => toast.show({ kind: "error", message: t("gallery.toastMessage") })}
        >
          {t("gallery.toastError")}
        </button>
      </div>

      <div className="flex flex-wrap items-center gap-6">
        <Progress value={64} label={t("gallery.progress")} speedBps={8_400_000} etaSeconds={42} />
        <Progress value={64} variant="circular" label={t("gallery.progress")} />
        <Progress value={100} variant="circular" label={t("gallery.progress")} />
        <Sparkline samples={samples} max={100} format={(v) => `${v.toFixed(0)}%`} />
        <span className="text-caption text-fg-secondary">{t("gallery.sparkline")}</span>
      </div>

      <Menu
        entries={[
          { id: ID.restart, label: t("gallery.menuRestart"), onSelect: () => undefined },
          { type: "separator", id: ID.sep },
          {
            id: ID.remove,
            label: t("gallery.menuRemove"),
            onSelect: () => undefined,
            destructive: true,
          },
        ]}
      >
        <Card>{t("gallery.menuHint")}</Card>
      </Menu>

      <div className="flex flex-col gap-2">
        <h3 className="text-headline">{t("gallery.list")}</h3>
        <Card padding="none" className="h-72">
          <VirtualList
            label={t("gallery.list")}
            items={rows}
            getKey={(n) => String(n)}
            renderRow={(n) => (
              <ListRow
                title={t("gallery.row", { index: n })}
                subtitle={t("gallery.rowSub")}
                trailing={String(n)}
              />
            )}
          />
        </Card>
      </div>

      <div className="flex flex-col gap-2">
        <button
          type="button"
          className={`${BUTTON} self-start`}
          onClick={() => {
            setLoading((v) => !v);
          }}
        >
          {t("gallery.skeletonToggle")}
        </button>
        <SkeletonSlot loading={loading} height={48} label={t("gallery.skeleton")}>
          <ListRow title={t("gallery.row", { index: 1 })} subtitle={t("gallery.rowSub")} />
        </SkeletonSlot>
        <Skeleton height={12} width={160} />
      </div>

      <Card>
        <EmptyState
          title={t("gallery.emptyTitle")}
          description={t("gallery.emptyBody")}
          action={
            <button type="button" className={BUTTON}>
              {t("gallery.emptyAction")}
            </button>
          }
        />
      </Card>

      <ToastViewport />
    </section>
  );
}
