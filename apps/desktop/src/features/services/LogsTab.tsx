import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { FOCUS_RING } from "../../design/primitives/shared";
import { TextField } from "../../design/primitives/TextField";
import type { ForgeApiError } from "../../ipc/errors";
import { toForgeApiError } from "../../ipc/errors";
import type { LogLineDto } from "../../ipc/generated";
import { copyText } from "../../ipc/native";
import { useForgeApi } from "../../state";

/** Lines read first and lines kept in memory while following. */
export const LOG_BACKLOG = 200;
export const LOG_KEEP = 1000;
const READ_METHOD = "logs.read";

export interface LogsTabProps {
  /** `mysql/default`: the target `logs.read` and `logs.subscribe` take. */
  target: string;
}

function append(lines: readonly LogLineDto[], more: readonly LogLineDto[]): LogLineDto[] {
  const last = lines[lines.length - 1]?.seq ?? -1;
  const fresh = more.filter((line) => line.seq > last);
  const all = fresh.length === 0 ? lines : [...lines, ...fresh];
  return all.length > LOG_KEEP ? all.slice(all.length - LOG_KEEP) : [...all];
}

/**
 * Minimal tail of one service's log (M2-E2-T12): the last lines from `logs.read`, then the
 * live ones from `logs.subscribe` resumed at `last_seq`. Filter, pause (lines keep arriving
 * into a buffer and are shown on resume) and copy. M2-E3-T12 replaces it with the shared
 * LogViewer.
 */
export function LogsTab({ target }: LogsTabProps) {
  const { t } = useTranslation("services");
  const api = useForgeApi();
  const [lines, setLines] = useState<readonly LogLineDto[]>([]);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const [loading, setLoading] = useState(true);
  const [lost, setLost] = useState(0);
  const [filter, setFilter] = useState("");
  const [paused, setPaused] = useState(false);
  const [copied, setCopied] = useState<"idle" | "copied" | "failed">("idle");
  const pausedRef = useRef(false);
  const buffer = useRef<LogLineDto[]>([]);
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // An object: the flag flips from the cleanup while the async steps below read it.
    const run = { alive: true, unsubscribe: null as (() => Promise<void>) | null };
    const isAlive = () => run.alive;
    api.call("logs.read", { target, limit: LOG_BACKLOG }).then(
      async (chunk) => {
        if (!run.alive) return;
        setLines(chunk.lines);
        setLoading(false);
        try {
          const handle = await api.subscribe(
            "logs.subscribe",
            { target, resume_from: chunk.last_seq },
            (entries) => {
              const incoming: LogLineDto[] = [];
              for (const entry of entries) {
                if (entry.kind === "item") incoming.push(entry.item.line);
                else if (entry.kind === "lagged") setLost((n) => n + entry.missed);
              }
              if (incoming.length === 0) return;
              if (pausedRef.current) buffer.current = append(buffer.current, incoming);
              else setLines((current) => append(current, incoming));
            },
          );
          if (isAlive()) run.unsubscribe = () => handle.unsubscribe();
          else void handle.unsubscribe().catch(() => undefined);
        } catch {
          // The backlog is still useful; the live tail is best effort (the daemon may not
          // implement the stream yet).
        }
      },
      (error: unknown) => {
        if (!run.alive) return;
        setFailure(toForgeApiError(error, { method: READ_METHOD }));
        setLoading(false);
      },
    );
    return () => {
      run.alive = false;
      void run.unsubscribe?.().catch(() => undefined);
    };
  }, [api, target]);

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return needle === "" ? lines : lines.filter((line) => line.text.toLowerCase().includes(needle));
  }, [lines, filter]);

  useEffect(() => {
    const el = scroller.current;
    if (el !== null && !paused) el.scrollTop = el.scrollHeight;
  }, [visible, paused]);

  const togglePause = useCallback(() => {
    const next = !pausedRef.current;
    pausedRef.current = next;
    setPaused(next);
    if (!next && buffer.current.length > 0) {
      const pending = buffer.current;
      buffer.current = [];
      setLines((current) => append(current, pending));
    }
  }, []);

  const copy = async () => {
    const ok = await copyText(visible.map((line) => line.text).join("\n"));
    setCopied(ok ? "copied" : "failed");
    setTimeout(() => {
      setCopied("idle");
    }, 1500);
  };

  if (failure !== null) {
    return (
      <ErrorNotice
        error={failure}
        context={{ method: READ_METHOD, params: { target, limit: LOG_BACKLOG } }}
      />
    );
  }
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-end gap-2">
        <TextField
          label={t("logs.filter")}
          value={filter}
          width="sm"
          onChange={(event) => {
            setFilter(event.target.value);
          }}
        />
        <Button onClick={togglePause} aria-pressed={paused}>
          {paused ? t("logs.resume") : t("logs.pause")}
        </Button>
        <Button
          disabled={visible.length === 0}
          onClick={() => {
            void copy();
          }}
        >
          {copied === "copied"
            ? t("logs.copied")
            : copied === "failed"
              ? t("logs.copyFailed")
              : t("logs.copy")}
        </Button>
      </div>
      {lost > 0 ? (
        <p role="status" className="text-caption text-state-starting">
          {t("logs.lost", { count: lost })}
        </p>
      ) : null}
      <div
        ref={scroller}
        role="log"
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- a scrollable region must take focus (axe scrollable-region-focusable)
        tabIndex={0}
        aria-label={t("logs.region", { name: target })}
        data-testid="log-lines"
        className={`h-80 overflow-auto rounded-control border border-stroke bg-surface p-2 font-mono text-mono ${FOCUS_RING}`}
      >
        {visible.length === 0 ? (
          <p className="font-sans text-callout text-fg-secondary">
            {loading ? t("logs.loading") : t("logs.empty")}
          </p>
        ) : (
          visible.map((line) => (
            <div
              key={line.seq}
              data-level={line.level ?? undefined}
              className="break-all whitespace-pre-wrap text-fg-primary"
            >
              {line.text}
            </div>
          ))
        )}
      </div>
      {paused ? (
        <p role="status" className="text-caption text-fg-secondary">
          {t("logs.paused")}
        </p>
      ) : null}
    </div>
  );
}
