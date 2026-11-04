// "Atenção" (docs/07 section 5.2): rendered only when a registered source reports something.
// Sources are hooks, so each one lives in its own probe component (a source added later never
// changes the number of hooks of a render) and reports its items up; the section itself is not
// in the DOM while every source is empty.
import { useNavigate } from "@tanstack/react-router";
import { Component, useCallback, useLayoutEffect, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  attentionSources,
  useSlot,
  type AttentionItem,
  type AttentionSource,
} from "../../app/slots";
import { Badge, type BadgeTone } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ERRORS_NS, type I18nText } from "../../ipc/describeError";

type Report = (id: string, items: readonly AttentionItem[]) => void;

/** A source that throws is dropped, never the screen. */
class ProbeBoundary extends Component<{ id: string; children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  override componentDidCatch(error: unknown) {
    console.error(`attention source ${this.props.id} failed`, error);
  }
  override render() {
    return this.state.failed ? null : this.props.children;
  }
}

function Probe({ source, report }: { source: AttentionSource; report: Report }) {
  const items = source.useItems();
  // Compare by content: a source may rebuild equal arrays on every render.
  const key = JSON.stringify(items);
  useLayoutEffect(() => {
    report(source.id, items);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `key` stands for `items`.
  }, [key, report, source.id]);
  useLayoutEffect(
    () => () => {
      report(source.id, []);
    },
    [report, source.id],
  );
  return null;
}

const TONE: Record<AttentionItem["severity"], BadgeTone> = {
  info: "info",
  warning: "warning",
  critical: "danger",
};

function useText() {
  const { t } = useTranslation();
  return (text: I18nText, ns: string): string =>
    (t as unknown as (key: string, options: object) => string)(text.key, { ns, ...text.values });
}

function Item({ item }: { item: AttentionItem }) {
  const { t } = useTranslation("dashboard");
  const navigate = useNavigate();
  const text = useText();
  const ns = item.ns ?? ERRORS_NS;
  const href = item.href;
  return (
    <li
      data-attention-item={item.id}
      data-severity={item.severity}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <p className="text-body font-semibold text-fg-primary">{text(item.title, ns)}</p>
          {item.cause === undefined ? null : (
            <p className="text-callout text-fg-secondary">{text(item.cause, ns)}</p>
          )}
        </div>
        <Badge tone={TONE[item.severity]}>{t(`attention.severity.${item.severity}`)}</Badge>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        {href === undefined || item.actionLabel === undefined ? null : (
          <Button
            onClick={() => {
              void navigate({
                to: href,
                ...(item.search === undefined ? {} : { search: item.search }),
              } as unknown as Parameters<typeof navigate>[0]);
            }}
          >
            {text(item.actionLabel, ns)}
          </Button>
        )}
        {item.cli === undefined ? null : (
          <div className="min-w-0 flex-1">
            <CliCommand command={item.cli} />
          </div>
        )}
      </div>
    </li>
  );
}

export function AttentionSection() {
  const { t } = useTranslation("dashboard");
  const sources = useSlot(attentionSources);
  const [reports, setReports] = useState<Readonly<Record<string, readonly AttentionItem[]>>>({});
  const report = useCallback<Report>((id, items) => {
    setReports((previous) => {
      const before = previous[id] ?? [];
      if (before.length === 0 && items.length === 0) return previous;
      return { ...previous, [id]: items };
    });
  }, []);
  const items = useMemo(
    () =>
      sources.flatMap((source) =>
        (reports[source.id] ?? []).map((item) => ({ ...item, id: `${source.id}/${item.id}` })),
      ),
    [sources, reports],
  );
  return (
    <>
      {sources.map((source) => (
        <ProbeBoundary key={source.id} id={source.id}>
          <Probe source={source} report={report} />
        </ProbeBoundary>
      ))}
      {items.length === 0 ? null : (
        <section aria-labelledby="dashboard-attention" data-testid="dashboard-attention">
          <h2 id="dashboard-attention" className="pb-2 text-headline">
            {t("attention.title")}
          </h2>
          <ul className="flex flex-col gap-2">
            {items.map((item) => (
              <Item key={item.id} item={item} />
            ))}
          </ul>
        </section>
      )}
    </>
  );
}
