// Machine monitor of the Dashboard (docs/14 section 4.2): CPU, RAM and disk of the machine
// against the Forgebox share, from `stats.top` -> `TopDto.machine`, refreshed every 5 s while
// the window is visible (`useTop`). Disks are shown by role (root/data), never by path.
import { useTranslation } from "react-i18next";
import { AsyncView } from "../../design/primitives/AsyncView";
import { CliCommand } from "../../design/primitives/CliCommand";
import { renderCli } from "../../ipc/cli";
import type * as T from "../../ipc/generated";
import { useCapability, useConnectionStatus, useRuntime } from "../../state";
import { useMetricsFormatters, useTop } from "./state";

const BAR_HEIGHT = 8;
/** Fixed in every state, so swapping them never moves the Dashboard. */
const MACHINE_HEIGHT = 128;
const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const TOP_CONTEXT = { method: "stats.top", params: {} } as const;
const CPU_ID = "machine-cpu";
const RAM_ID = "machine-ram";

/** Percent of `part` in `whole`, clamped to 0..100; 0 for an empty whole. */
export function share(part: number, whole: number): number {
  if (!(whole > 0) || !Number.isFinite(part)) return 0;
  return Math.min(100, Math.max(0, (part / whole) * 100));
}

interface MeterProps {
  label: string;
  /** Used part of the whole, percent. */
  used: number;
  /** The Forgebox part of `used`, percent (never more than `used`). */
  mine: number | null;
  summary: string;
  detail?: string;
  testId: string;
}

function Meter({ label, used, mine, summary, detail, testId }: MeterProps) {
  const ownShare = mine === null ? 0 : Math.min(mine, used);
  return (
    <div data-testid={testId} className="flex min-w-0 flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-callout text-fg-secondary">{label}</span>
        <span className="text-body text-fg-primary tabular-nums">{summary}</span>
      </div>
      <div
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(used)}
        aria-valuetext={detail === undefined ? summary : `${summary}. ${detail}`}
        style={{ height: BAR_HEIGHT }}
        className="relative overflow-hidden rounded-full bg-fill"
      >
        <div
          className="absolute inset-y-0 left-0 rounded-full bg-fg-tertiary"
          style={{ width: `${String(used)}%` }}
        />
        <div
          data-forgebox-share=""
          className="absolute inset-y-0 left-0 rounded-full bg-accent"
          style={{ width: `${String(ownShare)}%` }}
        />
      </div>
      {detail === undefined ? null : (
        <span className="text-callout text-fg-secondary tabular-nums">{detail}</span>
      )}
    </div>
  );
}

function Machine({ machine }: { machine: T.MachineStatsDto }) {
  const { t } = useTranslation("metrics");
  const f = useMetricsFormatters();
  const memUsed = Math.max(0, machine.mem_total - machine.mem_available);
  return (
    <div className="grid grid-cols-1 gap-4 p-3 md:grid-cols-3">
      <Meter
        testId={CPU_ID}
        label={t("machine.cpu")}
        used={machine.cpu_pct ?? 0}
        mine={machine.forgebox_cpu_pct}
        summary={f.percent(machine.cpu_pct)}
        detail={t("machine.forgebox", { value: f.percent(machine.forgebox_cpu_pct) })}
      />
      <Meter
        testId={RAM_ID}
        label={t("machine.ram")}
        used={share(memUsed, machine.mem_total)}
        mine={share(machine.forgebox_rss, machine.mem_total)}
        summary={t("machine.ofTotal", {
          used: f.bytes(memUsed),
          total: f.bytes(machine.mem_total),
        })}
        detail={t("machine.forgebox", { value: f.bytes(machine.forgebox_rss) })}
      />
      {machine.disks.map((disk) => (
        <Meter
          key={disk.role}
          testId={`machine-disk-${disk.role}`}
          label={t("machine.disk", { role: t(`machine.roles.${disk.role}`) })}
          used={share(disk.total - disk.available, disk.total)}
          mine={null}
          summary={t("machine.free", { value: f.bytes(disk.available) })}
          detail={t("machine.diskTotal", { total: f.bytes(disk.total) })}
        />
      ))}
    </div>
  );
}

export function MachineMonitor() {
  const { t } = useTranslation("metrics");
  const runtime = useRuntime();
  const status = useConnectionStatus();
  const capability = useCapability("stats.top");
  const top = useTop();
  const machine = top.data?.machine;
  const away = AWAY.has(status);
  const unavailable = !capability.available || (top.data !== undefined && machine == null);
  // Same states as the Projetos section (docs/07 section 11): daemon away, loading, error with
  // cause and retry, unavailable; never an endless "loading" when `stats.top` fails.
  const loading = !away && !unavailable && top.isPending;
  return (
    <section aria-labelledby="dashboard-machine" data-testid="dashboard-machine">
      <div className="flex flex-wrap items-center gap-3 pb-2">
        <h2 id="dashboard-machine" className="text-headline">
          {t("machine.title")}
        </h2>
        <div className="ml-auto min-w-0">
          <CliCommand command={renderCli("stats.top", {})} />
        </div>
      </div>
      <div className="rounded-card border border-stroke bg-surface">
        <AsyncView
          height={MACHINE_HEIGHT}
          loading={loading}
          disconnected={away}
          onStartDaemon={() => {
            void runtime.api.start();
          }}
          {...(top.error === null || unavailable ? {} : { error: top.error })}
          errorContext={TOP_CONTEXT}
          onRetry={() => {
            void top.refetch();
          }}
          empty={unavailable || machine == null}
          emptyTitle={t("machine.unavailable")}
        >
          {machine == null ? null : <Machine machine={machine} />}
        </AsyncView>
      </div>
    </section>
  );
}
