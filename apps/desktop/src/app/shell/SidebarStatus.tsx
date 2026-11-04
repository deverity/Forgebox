import { Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { useFormatters } from "../../design/primitives/useFormatters";
import type { ConnectionStatus } from "../../ipc/bridge";
import { useConnectionStatus, useStatusTotals, useSystem } from "../../state";

type Tone = "running" | "starting" | "stopped";

// Static dots: the animated StatusDot (motion) stays out of the first paint; CSS pulses instead.
const DOT: Record<ConnectionStatus, Tone> = {
  connected: "running",
  connecting: "starting",
  starting: "starting",
  reconnecting: "starting",
  stopped: "stopped",
  closed: "stopped",
  uninitialized: "stopped",
  idle: "stopped",
};

const TONE_CLASS: Record<Tone, string> = {
  running: "bg-state-running",
  starting: "bg-state-starting motion-safe:animate-pulse",
  stopped: "bg-state-stopped",
};

const SEPARATOR = " · ";
const MIDDLE_DOT = "·";
const DIAGNOSTICS = "/diagnostics";
const DNS_TESTID = "status-dns";
const CA_TESTID = "status-ca";

/** Soft pill (low-alpha fill, no outline) that leads to Diagnostics. */
const CHIP = cx(
  "inline-flex h-5 items-center rounded-full border border-stroke-control bg-fill px-2 text-caption text-fg-secondary hover:bg-fill-hover",
  FOCUS_RING,
);

type DnsKey = "unknown" | "off" | "ok" | "attention";
type CaKey = "unknown" | "missing" | "ok" | "untrusted";

/** Everything the footer shows, as text (the collapsed sidebar speaks it as one label). */
function useStatusText() {
  const { t } = useTranslation("shell");
  const fmt = useFormatters();
  const status = useConnectionStatus();
  const totals = useStatusTotals();
  const dns = useSystem((s) => s.dns);
  const ca = useSystem((s) => s.ca);
  const dash = t("status.unknownValue");

  const dnsKey: DnsKey =
    dns === null ? "unknown" : !dns.enabled ? "off" : dns.server_running ? "ok" : "attention";
  const caKey: CaKey =
    ca === null
      ? "unknown"
      : !ca.exists
        ? "missing"
        : ca.trusted_current_user || ca.trusted_local_machine
          ? "ok"
          : "untrusted";
  const resources =
    t("status.cpu", { value: totals === null ? dash : fmt.percent(totals.cpu / 100, 1) }) +
    SEPARATOR +
    t("status.ram", { value: totals === null ? dash : fmt.bytes(totals.rss) });
  return {
    status,
    connection: t(`status.connection.${status}`),
    resources,
    dnsKey,
    dns: t(`status.dns.${dnsKey}`),
    caKey,
    ca: t(`status.ca.${caKey}`),
  };
}

/**
 * Sidebar footer (M2-E1-T23, replaces the bottom status bar): daemon connection and CPU · RAM
 * (the daemon plus the stack it runs, docs/07 section 4) on one compact line, then the DNS and
 * CA chips, which lead to Diagnostics. Unknown values show a dash, never an invented zero.
 * Collapsed (56 px): one dot links to Diagnostics and speaks everything in its name.
 */
export function SidebarStatus({ collapsed }: { collapsed: boolean }) {
  const { t } = useTranslation("shell");
  const s = useStatusText();
  const dot = (
    <span
      aria-hidden="true"
      data-tone={DOT[s.status]}
      className={`size-2 shrink-0 rounded-full ${TONE_CLASS[DOT[s.status]]}`}
    />
  );

  if (collapsed) {
    const summary = [s.connection, s.resources, s.dns, s.ca].join(SEPARATOR);
    return (
      <section aria-label={t("status.label")} data-testid="sidebar-status">
        <Link
          to={DIAGNOSTICS}
          aria-label={summary}
          title={summary}
          data-testid={DNS_TESTID}
          data-state={s.dnsKey}
          className={cx(
            "relative mx-auto flex size-8 items-center justify-center rounded-item hover:bg-fill-hover",
            FOCUS_RING,
          )}
        >
          {dot}
          {/* Same test ids as the expanded footer; the link's name is what is announced. */}
          <span aria-hidden="true" className="sr-only" data-testid="status-connection">
            {s.connection}
          </span>
          <span aria-hidden="true" className="sr-only" data-testid="status-resources">
            {s.resources}
          </span>
          <span aria-hidden="true" className="sr-only" data-testid={CA_TESTID} data-state={s.caKey}>
            {s.ca}
          </span>
        </Link>
      </section>
    );
  }

  return (
    <section
      aria-label={t("status.label")}
      data-testid="sidebar-status"
      className="flex flex-col gap-1.5 px-2 text-caption text-fg-secondary"
    >
      <p className="m-0 flex flex-wrap items-center gap-x-1.5">
        <span className="inline-flex items-center gap-1.5" data-testid="status-connection">
          {dot}
          <span>{s.connection}</span>
        </span>
        <span aria-hidden="true">{MIDDLE_DOT}</span>
        <span
          className="tabular-nums"
          title={t("status.resourcesHint")}
          data-testid="status-resources"
        >
          {s.resources}
        </span>
      </p>
      <p className="m-0 flex flex-wrap gap-1">
        <Link to={DIAGNOSTICS} className={CHIP} data-testid={DNS_TESTID} data-state={s.dnsKey}>
          {s.dns}
        </Link>
        <Link to={DIAGNOSTICS} className={CHIP} data-testid={CA_TESTID} data-state={s.caKey}>
          {s.ca}
        </Link>
      </p>
    </section>
  );
}
