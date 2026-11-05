import { AnimatePresence, motion } from "motion/react";
import { useEffect, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { toast, toastStore, type ToastItem } from "./toastStore";
import { surfaceMotion } from "./useSurfaceMotion";

const TIMES = "×";

const KIND_BAR = {
  info: "bg-state-updating",
  success: "bg-state-running",
  error: "bg-state-failed",
} as const;

const subscribe = (listener: () => void) => toastStore.subscribe(listener);
const getSnapshot = () => toastStore.getSnapshot();

function ToastCard({ item }: { item: ToastItem }) {
  const { t } = useTranslation();
  const [paused, setPaused] = useState(false);
  const m = surfaceMotion("toast");

  // Auto-dismiss; hovering or focusing the toast pauses the timer so the user can read/undo.
  useEffect(() => {
    if (item.durationMs <= 0 || paused) return;
    const timer = setTimeout(() => {
      toast.dismiss(item.id);
    }, item.durationMs);
    return () => {
      clearTimeout(timer);
    };
  }, [item.id, item.durationMs, paused]);

  return (
    <motion.div
      layout={!m.reduced}
      data-motion={m.dataMotion}
      data-kind={item.kind}
      className="pointer-events-auto flex items-stretch overflow-hidden rounded-sheet border border-stroke bg-surface-2 text-body text-fg-primary shadow-popover"
      initial={m.initial}
      animate={m.animate}
      exit={m.exit}
      transition={m.transition}
      onMouseEnter={() => {
        setPaused(true);
      }}
      onMouseLeave={() => {
        setPaused(false);
      }}
      onFocus={() => {
        setPaused(true);
      }}
      onBlur={() => {
        setPaused(false);
      }}
    >
      <span aria-hidden="true" className={`w-1 shrink-0 ${KIND_BAR[item.kind]}`} />
      <p className="flex-1 px-3 py-2">{item.message}</p>
      <div className="flex items-center gap-1 pr-2">
        {item.undo ? (
          <button
            type="button"
            className="rounded-control px-2 py-1 text-accent hover:bg-surface focus-visible:outline-2 focus-visible:outline-accent"
            onClick={() => {
              item.undo?.onUndo();
              toast.dismiss(item.id);
            }}
          >
            {item.undo.label ?? t("ui.toastUndo")}
          </button>
        ) : null}
        <button
          type="button"
          aria-label={t("ui.toastDismiss")}
          className="rounded-control px-2 py-1 text-fg-secondary hover:bg-surface focus-visible:outline-2 focus-visible:outline-accent"
          onClick={() => {
            toast.dismiss(item.id);
          }}
        >
          <span aria-hidden="true">{TIMES}</span>
        </button>
      </div>
    </motion.div>
  );
}

/**
 * Mount once near the root. The two live regions are always in the DOM (screen readers only
 * announce changes to regions that already exist): `status` is polite for info/success,
 * `alert` is assertive for errors.
 */
export function ToastViewport() {
  const { t } = useTranslation();
  const items = useSyncExternalStore(subscribe, getSnapshot);
  const polite = items.filter((i) => i.kind !== "error");
  const assertive = items.filter((i) => i.kind === "error");

  return (
    <section
      aria-label={t("ui.toastRegion")}
      className="pointer-events-none fixed right-4 bottom-4 z-50 flex w-96 max-w-full flex-col gap-2"
    >
      <div role="alert" aria-live="assertive" className="flex flex-col gap-2">
        <AnimatePresence initial={false}>
          {assertive.map((item) => (
            <ToastCard key={item.id} item={item} />
          ))}
        </AnimatePresence>
      </div>
      <div role="status" aria-live="polite" className="flex flex-col gap-2">
        <AnimatePresence initial={false}>
          {polite.map((item) => (
            <ToastCard key={item.id} item={item} />
          ))}
        </AnimatePresence>
      </div>
    </section>
  );
}

export { toast };
