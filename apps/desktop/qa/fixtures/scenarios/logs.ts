// Handlers of the Logs screen (M2-E3-T12): `logs.targets` and `logs.read` (with `before_seq`)
// over a deterministic history. Live lines come from `FakeBridge.emitLog`; Playwright floods a
// target at 1,000 lines/s from the page (`QaHandle.bridge`) to measure long tasks and DOM rows.
import type { FakeBridge, FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";

export const QA_LOG_TARGETS: readonly T.LogTargetDto[] = [
  { target: "daemon", label: "Forgebox daemon", kind: "daemon", group: "daemon", live: true },
  {
    target: "nginx.error",
    label: "nginx · error",
    kind: "instance_file",
    group: "nginx",
    format: "nginx-error",
    live: true,
  },
  { target: "nginx", label: "nginx", kind: "instance", group: "nginx", live: true },
  { target: "mysql/default", label: "mysql", kind: "instance", group: "mysql", live: true },
  { target: "php@8.4", label: "PHP 8.4 pool", kind: "pool", group: "php", live: true },
  {
    target: "site:loja.test",
    label: "loja.test · access",
    kind: "site",
    group: "loja.test",
    live: true,
  },
  {
    target: "app:loja.test/web",
    label: "loja.test · web",
    kind: "app",
    group: "loja.test",
    live: true,
  },
];

/** Lines each target has before anything is emitted live. */
export const QA_LOG_HISTORY = 600;
const BASE_MS = Date.UTC(2026, 0, 1, 10, 0, 0);

/** Line `seq` (1-based) of `target` in the history. */
export function historyLine(target: string, seq: number): T.LogLineDto {
  const ts = new Date(BASE_MS + seq * 1000).toISOString();
  if (target.startsWith("site:")) {
    const host = target.slice("site:".length);
    const status = seq % 11 === 0 ? 502 : 200;
    return {
      seq,
      ts,
      source: "file",
      level: status >= 500 ? "error" : "info",
      text: `${host} GET /page-${String(seq)} ${String(status)}`,
      access: {
        host,
        status,
        method: "GET",
        uri: `/page-${String(seq)}`,
        rt_ms: 3.5,
        upstream: "127.0.0.1:19083",
      },
    };
  }
  return {
    seq,
    ts,
    source: "stdout",
    level: seq % 7 === 0 ? "warn" : "info",
    text: `${target} line ${String(seq)}`,
  };
}

export function logsHandlers(): FakeHandlers {
  return {
    "logs.targets": () => ({ targets: [...QA_LOG_TARGETS] }),
    "logs.read": (params) => {
      const limit = params.limit ?? 200;
      const before = params.before_seq ?? QA_LOG_HISTORY + 1;
      const last = Math.min(before - 1, QA_LOG_HISTORY);
      const first = Math.max(1, last - limit + 1);
      const lines: T.LogLineDto[] = [];
      for (let seq = first; seq <= last; seq += 1) lines.push(historyLine(params.target, seq));
      return { target: params.target, lines, last_seq: QA_LOG_HISTORY };
    },
  };
}

/** Lines seeded per target for the QA captures (the viewer's `tail` is 200). */
export const QA_LOG_SEED = 180;
const PHP_FILE = "C:/Forgebox/www/loja/app/Http/Controllers/CartController.php";

/**
 * Fills the `logs.subscribe` ring of every target once, before anyone subscribes, so a Logs
 * cell that follows a source shows lines (R-29, M2-E4-T10): levels, a PHP `file:line`, the access
 * columns of the site targets and enough lines for the virtualized list to scroll.
 */
export function seedLogLines(fake: FakeBridge): void {
  for (const { target } of QA_LOG_TARGETS) {
    for (let seq = 1; seq <= QA_LOG_SEED; seq += 1) {
      const line: Partial<T.LogLineDto> = historyLine(target, seq);
      delete line.seq;
      const withFile =
        seq % 13 === 0 && !target.startsWith("site:")
          ? {
              ...line,
              level: "error" as const,
              text: `PHP Fatal error: Uncaught TypeError in ${PHP_FILE}:${String(40 + (seq % 9))}`,
            }
          : line;
      fake.emitLog(target, withFile as Omit<T.LogLineDto, "seq">);
    }
  }
}
