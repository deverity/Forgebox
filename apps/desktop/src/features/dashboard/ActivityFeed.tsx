import { useTranslation } from "react-i18next";
import { FOCUS_RING } from "../../design/primitives/shared";
import { Skeleton } from "../../design/primitives/Skeleton";
import { useFormatters } from "../../design/primitives/useFormatters";
import type { EventDto } from "../../ipc/generated";
import { useTools } from "../../state";
import { humanizeEvent } from "../shared/activity/humanizeEvent";
import { useNow } from "../services/useNow";
import { useActivity } from "./useActivity";

const TONE_CLASS = {
  neutral: "bg-fg-tertiary",
  success: "bg-state-running",
  warning: "bg-state-starting",
  danger: "bg-state-failed",
} as const;

/** Atividade: the last events as sentences with a relative time ("PHP 8.3.37 instalado, ha 2 min"). */
export function ActivityFeed({ height }: { height: number }) {
  const { t } = useTranslation("dashboard");
  const format = useFormatters();
  // `now` ticks every 15 s: an event newer than the last tick reads "now", never "in 1 s".
  const now = useNow(15_000);
  const tools = useTools((state) => state.tools);
  const { events, loading } = useActivity();
  const toolName = (id: string) => tools[id]?.name ?? id;
  const rows = events.flatMap((event: EventDto) => {
    const human = humanizeEvent(event, { toolName });
    return human === null ? [] : [{ event, human }];
  });
  return (
    <section aria-labelledby="dashboard-activity" data-testid="dashboard-activity">
      <h2 id="dashboard-activity" className="pb-2 text-headline">
        {t("activity.title")}
      </h2>
      {/* Keyboard access to the scrollable list (axe: scrollable-region-focusable). */}
      <div
        role="region"
        aria-labelledby="dashboard-activity"
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scroll region, keyboard access
        tabIndex={0}
        style={{ height }}
        aria-busy={loading}
        className={`overflow-y-auto rounded-card border border-stroke bg-surface ${FOCUS_RING}`}
      >
        {loading ? (
          <div className="flex flex-col gap-2 p-3">
            <span className="sr-only">{t("activity.loading")}</span>
            {Array.from({ length: 6 }, (_, index) => (
              <Skeleton key={index} height={20} />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <p className="p-4 text-callout text-fg-secondary">{t("activity.empty")}</p>
        ) : (
          <ol className="flex flex-col">
            {rows.map(({ event, human }) => (
              <li
                key={event.id}
                data-activity-kind={event.kind}
                className="flex items-baseline gap-2 border-b border-stroke px-3 py-2 last:border-b-0"
              >
                <span
                  aria-hidden="true"
                  className={`mt-1.5 inline-block size-2 shrink-0 self-start rounded-full ${TONE_CLASS[human.tone]}`}
                />
                <span className="min-w-0 flex-1 truncate text-callout text-fg-primary">
                  {t(`activity.event.${human.key}` as "activity.event.daemon_ready", human.values)}
                </span>
                <time
                  dateTime={event.ts}
                  title={format.date(Date.parse(event.ts))}
                  className="shrink-0 text-caption text-fg-secondary"
                >
                  {format.relative(Math.min(Date.parse(event.ts), now), now)}
                </time>
              </li>
            ))}
          </ol>
        )}
      </div>
    </section>
  );
}
