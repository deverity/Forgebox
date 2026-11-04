import { useVirtualizer } from "@tanstack/react-virtual";
import { forwardRef, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Select } from "../../design/primitives/Select";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { Skeleton } from "../../design/primitives/Skeleton";
import { TextField } from "../../design/primitives/TextField";
import { toForgeApiError } from "../../ipc/errors";
import { copyText } from "../../ipc/native";
import { toast } from "../../design/primitives/toastStore";
import { useForgeApi, useHello } from "../../state";
import {
  LEVELS,
  NO_FILTERS,
  SINCE_OPTIONS,
  accessHosts,
  applyFilters,
  cliHint,
  clock,
  filtersActive,
  levelOf,
  splitText,
  statusTone,
  findFileLinks,
  type FileLink,
  type Filters,
  type LevelFilter,
  type LogRow,
  type SinceFilter,
} from "./model";
import { useLogStream } from "./useLogStream";

export const ROW_HEIGHT = 22;
const FOLLOW_SLACK = 24;
const ALL = "all";
const SUBSCRIBE = "logs.subscribe";
const COPIED_MS = 1500;

export interface LogViewerProps {
  /** `logs.targets` ids: `nginx.error`, `mysql/default`, `site:loja.test`, `app:loja.test/web`. */
  targets: readonly string[];
  /** Accessible name of the list (e.g. the service). */
  name: string;
  /** Height of the list in px; omitted = fills the parent (a flex column with a height). */
  height?: number;
}

const LEVEL_CLASS: Record<string, string> = {
  error: "text-state-failed",
  warn: "text-state-starting",
  debug: "text-fg-tertiary",
};

function Highlighted({
  text,
  filter,
  links,
  onOpen,
  remoteHint,
  openTitle,
}: {
  text: string;
  filter: string;
  links: boolean;
  onOpen: (link: FileLink) => void;
  remoteHint: string;
  openTitle: (link: FileLink) => string;
}) {
  const parts = splitText(text, filter, true);
  return (
    <>
      {parts.map((part, index) => {
        if (part.link !== undefined) {
          const link = part.link;
          if (!links) {
            return (
              <span key={index} title={remoteHint} data-remote-link="">
                {part.text}
              </span>
            );
          }
          return (
            <button
              key={index}
              type="button"
              title={openTitle(link)}
              data-file-link=""
              className={cx("cursor-pointer text-accent underline", FOCUS_RING)}
              onClick={() => {
                onOpen(link);
              }}
            >
              {part.text}
            </button>
          );
        }
        return part.match === true ? (
          <mark key={index} className="bg-accent/30 text-fg-primary">
            {part.text}
          </mark>
        ) : (
          <span key={index}>{part.text}</span>
        );
      })}
    </>
  );
}

/**
 * The shared log list (M2-E3-T12): follows `targets`, merges them by time, filters on the
 * client and keeps at most 20,000 rows, of which only the visible ones are in the DOM. Used by
 * the Logs screen, the Services "Logs" tab and the site "Processos" tab.
 */
export const LogViewer = forwardRef<HTMLDivElement, LogViewerProps>(function LogViewer(
  { targets, name, height },
  forwardedRef,
) {
  const { t } = useTranslation("logs");
  const api = useForgeApi();
  const hello = useHello();
  const local = hello?.local === true;
  const stream = useLogStream(targets);
  const { rows, arrived, paused, setPaused } = stream;
  const [filters, setFilters] = useState<Filters>(NO_FILTERS);
  const [follow, setFollow] = useState(true);
  const [seenAt, setSeenAt] = useState(0);
  const [selected, setSelected] = useState<ReadonlySet<number>>(new Set());
  const [anchor, setAnchor] = useState<number | null>(null);
  const [copied, setCopied] = useState<"idle" | "copied" | "failed">("idle");
  const scroller = useRef<HTMLDivElement>(null);
  const setScroller = useCallback(
    (node: HTMLDivElement | null) => {
      scroller.current = node;
      if (typeof forwardedRef === "function") forwardedRef(node);
      else if (forwardedRef) forwardedRef.current = node;
    },
    [forwardedRef],
  );

  const visible = useMemo(() => applyFilters(rows, filters, Date.now()), [rows, filters]);
  const hosts = useMemo(() => accessHosts(rows), [rows]);
  const lines = visible;

  // eslint-disable-next-line react-hooks/incompatible-library -- useVirtualizer is used as documented, results are not memoized
  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 10,
    initialRect: { width: 0, height: height ?? 480 },
    getItemKey: (index) => lines[index]?.id ?? index,
  });
  const total = virtualizer.getTotalSize();

  // Follow: keep the end in view as rows arrive.
  useEffect(() => {
    const el = scroller.current;
    if (el !== null && follow) el.scrollTop = el.scrollHeight;
  }, [total, follow]);

  // Leaving the end is a scroll *up*: content that grows between two frames moves the end
  // away without the reader scrolling, and must not stop the follow.
  const lastTop = useRef(0);
  const onScroll = () => {
    const el = scroller.current;
    if (el === null) return;
    const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight <= FOLLOW_SLACK;
    if (atEnd) {
      if (!follow) setFollow(true);
    } else if (follow && el.scrollTop < lastTop.current) {
      setFollow(false);
      setSeenAt(arrived);
    }
    lastTop.current = el.scrollTop;
  };

  const jumpToEnd = () => {
    setFollow(true);
    setPaused(false);
  };

  const newLines = follow && !paused ? 0 : Math.max(0, arrived - seenAt);

  const lostText = (count: number) => t("lost", { count });
  const openFile = useCallback(
    (link: FileLink) => {
      api
        .call("shell.open_editor", { path: link.path, line: link.line })
        .catch((error: unknown) => {
          const failure = toForgeApiError(error, { method: "shell.open_editor" });
          toast.show({ kind: "error", message: `${t("openFailed")} ${failure.message}` });
        });
    },
    [api, t],
  );

  const setFilter = <K extends keyof Filters>(key: K, value: Filters[K]) => {
    setFilters((current) => ({ ...current, [key]: value }));
  };

  const select = (row: LogRow, extend: boolean) => {
    setSelected((current) => {
      const next = new Set(current);
      if (extend && anchor !== null) {
        const from = lines.findIndex((r) => r.id === anchor);
        const to = lines.findIndex((r) => r.id === row.id);
        if (from >= 0 && to >= 0) {
          for (let i = Math.min(from, to); i <= Math.max(from, to); i += 1) {
            const item = lines[i];
            if (item !== undefined) next.add(item.id);
          }
          return next;
        }
      }
      if (next.has(row.id)) next.delete(row.id);
      else next.add(row.id);
      return next;
    });
    setAnchor(row.id);
  };

  const selectedRows = lines.filter((row) => selected.has(row.id));
  const openable = selectedRows
    .map((row) => (row.kind === "line" ? findFileLinks(row.line.text)[0]?.link : undefined))
    .find((link) => link !== undefined);

  const copy = async () => {
    const native = globalThis.getSelection()?.toString() ?? "";
    const source = selectedRows.length > 0 ? selectedRows : native !== "" ? null : lines;
    const text =
      source === null
        ? native
        : source
            .map((row) => (row.kind === "gap" ? lostText(row.missed) : row.line.text))
            .join("\n");
    const ok = await copyText(text);
    setCopied(ok ? "copied" : "failed");
    setTimeout(() => {
      setCopied("idle");
    }, COPIED_MS);
  };

  const listHeight = height;
  const sizeStyle = listHeight === undefined ? undefined : { height: listHeight };
  const remoteHint = t("remoteNote");
  const empty = !stream.loading && lines.length === 0;
  const hint = targets[0] === undefined ? null : cliHint(targets[0], filters.text);

  const levelOptions = LEVELS.map((level) => ({
    value: level,
    label: t(`filter.levels.${level}`),
  }));
  const sinceOptions = SINCE_OPTIONS.map((since) => ({
    value: since,
    label: t(since === "all" ? "filter.all" : `filter.${since}`),
  }));
  const siteOptions = [
    { value: ALL, label: t("filter.allSites") },
    ...hosts.map((host) => ({ value: host, label: host })),
  ];

  return (
    <div
      className={cx("flex min-h-0 flex-col gap-2", height === undefined && "h-full")}
      data-testid="log-viewer"
    >
      <div className="flex flex-wrap items-end gap-2">
        <TextField
          label={t("filter.text")}
          value={filters.text}
          width="sm"
          onChange={(event) => {
            setFilter("text", event.target.value);
          }}
        />
        <Select
          label={t("filter.level")}
          width="sm"
          options={levelOptions}
          value={filters.level}
          onValueChange={(value) => {
            setFilter("level", value as LevelFilter);
          }}
        />
        {hosts.length > 0 ? (
          <Select
            label={t("filter.site")}
            width="sm"
            options={siteOptions}
            value={filters.site}
            onValueChange={(value) => {
              setFilter("site", value);
            }}
          />
        ) : null}
        <Select
          label={t("filter.since")}
          width="sm"
          options={sinceOptions}
          value={filters.since}
          onValueChange={(value) => {
            setFilter("since", value as SinceFilter);
          }}
        />
        {filtersActive(filters) ? (
          <Button
            variant="ghost"
            onClick={() => {
              setFilters(NO_FILTERS);
            }}
          >
            {t("filter.clear")}
          </Button>
        ) : null}
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <Button
            disabled={!stream.hasEarlier || rows.length === 0}
            loading={stream.loadingEarlier}
            loadingLabel={t("loadingEarlier")}
            title={stream.hasEarlier ? undefined : t("noEarlier")}
            onClick={() => {
              void stream.loadEarlier();
            }}
          >
            {t("earlier")}
          </Button>
          <Button
            onClick={() => {
              if (!paused) setSeenAt(arrived);
              setPaused(!paused);
            }}
            aria-pressed={paused}
          >
            {paused ? t("resume") : t("pause")}
          </Button>
          <Button
            disabled={lines.length === 0}
            onClick={() => {
              void copy();
            }}
          >
            {copied === "copied"
              ? t("copied")
              : copied === "failed"
                ? t("copyFailed")
                : selectedRows.length > 0
                  ? t("copySelection")
                  : t("copy")}
          </Button>
          {local ? (
            <span
              title={openable === undefined ? t("openFileNone") : undefined}
              className="inline-flex"
            >
              <Button
                disabled={openable === undefined}
                onClick={() => {
                  if (openable !== undefined) openFile(openable);
                }}
              >
                {t("openFile")}
              </Button>
            </span>
          ) : null}
        </div>
      </div>
      {!local && hello !== null ? (
        <p className="text-caption text-fg-tertiary" data-testid="logs-remote-note">
          {remoteHint}
        </p>
      ) : null}
      {stream.failure !== null ? (
        <ErrorNotice
          error={stream.failure}
          context={{ method: SUBSCRIBE, params: { target: targets[0] ?? "" } }}
        />
      ) : null}
      <div className="relative min-h-0 flex-1" style={sizeStyle}>
        {stream.loading ? (
          <div aria-busy="true" data-testid="logs-loading" className="h-full">
            <span className="sr-only">{t("loading")}</span>
            <Skeleton height={listHeight ?? 320} />
          </div>
        ) : (
          <div
            ref={setScroller}
            role="log"
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- a scrollable region must take focus (axe scrollable-region-focusable)
            tabIndex={0}
            aria-label={t("region", { name })}
            data-testid="log-lines"
            onScroll={onScroll}
            className={cx(
              "h-full overflow-auto rounded-control border border-stroke bg-surface font-mono text-mono",
              FOCUS_RING,
            )}
          >
            {empty ? (
              <p className="p-3 font-sans text-callout text-fg-secondary">
                {targets.length === 0
                  ? t("noTarget")
                  : filtersActive(filters)
                    ? t("emptyFiltered")
                    : t("empty")}
              </p>
            ) : (
              <div
                style={{
                  height: total,
                  position: "relative",
                  minWidth: "100%",
                  width: "max-content",
                }}
              >
                {virtualizer.getVirtualItems().map((item) => {
                  const row = lines[item.index];
                  if (row === undefined) return null;
                  return (
                    <div
                      key={item.key}
                      data-row={row.id}
                      style={{
                        position: "absolute",
                        top: 0,
                        left: 0,
                        height: ROW_HEIGHT,
                        transform: `translateY(${String(item.start)}px)`,
                      }}
                      className={cx(
                        "flex min-w-full items-center gap-2 px-1 whitespace-pre",
                        selected.has(row.id) && "bg-accent/15",
                      )}
                    >
                      <button
                        type="button"
                        aria-label={t("selectLine")}
                        aria-pressed={selected.has(row.id)}
                        onClick={(event) => {
                          select(row, event.shiftKey);
                        }}
                        className={cx(
                          "size-3 shrink-0 rounded-full border border-stroke-strong",
                          selected.has(row.id) && "bg-accent",
                          FOCUS_RING,
                        )}
                      />
                      {row.kind === "gap" ? (
                        <span role="status" data-gap="" className="font-sans text-state-starting">
                          {lostText(row.missed)}
                        </span>
                      ) : (
                        <LineBody
                          row={row}
                          filter={filters.text}
                          links={local}
                          onOpen={openFile}
                          remoteHint={remoteHint}
                          openTitle={(link) =>
                            t("openFileTitle", { path: link.path, line: link.line })
                          }
                          msLabel={(ms) => t("access.ms", { ms })}
                          multi={targets.length > 1}
                        />
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
        {newLines > 0 ? (
          <button
            type="button"
            onClick={jumpToEnd}
            data-testid="logs-new-badge"
            className={cx(
              "absolute right-4 bottom-3 rounded-full bg-accent px-3 py-1 text-callout text-fg-on-accent shadow-popover",
              FOCUS_RING,
            )}
          >
            {t("newLinesJump", { count: newLines })}
          </button>
        ) : null}
      </div>
      {paused ? (
        <p role="status" className="text-caption text-fg-secondary">
          {t("paused")}
        </p>
      ) : null}
      {hint !== null ? <CliCommand command={hint} /> : null}
    </div>
  );
});

function LineBody({
  row,
  filter,
  links,
  onOpen,
  remoteHint,
  openTitle,
  msLabel,
  multi,
}: {
  row: Extract<LogRow, { kind: "line" }>;
  filter: string;
  links: boolean;
  onOpen: (link: FileLink) => void;
  remoteHint: string;
  openTitle: (link: FileLink) => string;
  msLabel: (ms: string) => string;
  multi: boolean;
}) {
  const { line } = row;
  const level = levelOf(line);
  const tone = level === null ? "" : (LEVEL_CLASS[level] ?? "");
  const prefix = multi ? (
    <span className="shrink-0 text-fg-tertiary" data-col="target">
      {row.target}
    </span>
  ) : null;
  if (line.access) {
    const access = line.access;
    const status = statusTone(access);
    return (
      <>
        {prefix}
        <span className="w-16 shrink-0 text-fg-tertiary" data-col="time">
          {clock(line.ts)}
        </span>
        <span
          className={cx(
            "w-9 shrink-0",
            status === "error" && "text-state-failed",
            status === "warn" && "text-state-starting",
          )}
          data-col="status"
        >
          {access.status}
        </span>
        <span className="w-12 shrink-0" data-col="method">
          {access.method}
        </span>
        <span className="w-80 shrink-0 truncate" data-col="uri" title={access.uri}>
          <Highlighted
            text={access.uri}
            filter={filter}
            links={links}
            onOpen={onOpen}
            remoteHint={remoteHint}
            openTitle={openTitle}
          />
        </span>
        <span className="w-40 shrink-0 truncate text-fg-secondary" data-col="host">
          {access.host}
        </span>
        <span className="w-20 shrink-0 text-right text-fg-secondary" data-col="ms">
          {access.rt_ms == null ? "" : msLabel(access.rt_ms.toFixed(1))}
        </span>
        <span className="shrink-0 text-fg-tertiary" data-col="upstream">
          {access.upstream ?? ""}
        </span>
      </>
    );
  }
  return (
    <>
      {prefix}
      <span data-level={level ?? undefined} className={cx("text-fg-primary", tone)}>
        <Highlighted
          text={line.text}
          filter={filter}
          links={links}
          onOpen={onOpen}
          remoteHint={remoteHint}
          openTitle={openTitle}
        />
      </span>
    </>
  );
}
