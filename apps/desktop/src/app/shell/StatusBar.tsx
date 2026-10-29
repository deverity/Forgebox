import { Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { FOCUS_RING } from "../../design/primitives/shared";
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
const DIAGNOSTICS = "/diagnostics";
const DNS_TESTID = "status-dns";
const CA_TESTID = "status-ca";

const CHIP =
  `rounded-control px-1 text-fg-secondary ${FOCUS_RING} hover:bg-surface-2`;

/**
 * Bottom bar: daemon connection, CPU · RAM (the daemon plus the stack it runs, docs/07
 * section 4), DNS and CA. Unknown values show a dash, never an invented zero.
 */
export function StatusBar() {
  const { t } = useTranslation("shell");
  const fmt = useFormatters();
  const status = useConnectionStatus();
  const totals = useStatusTotals();
  const dns = useSystem((s) => s.dns);
  const ca = useSystem((s) => s.ca);
  const dash = t("status.unknownValue");

  const dnsKey =
    dns === null ? "unknown" : !dns.enabled ? "off" : dns.server_running ? "ok" : "attention";
  const caKey =
    ca === null
      ? "unknown"
      : !ca.exists
        ? "missing"
        : ca.trusted_current_user || ca.trusted_local_machine
          ? "ok"
          : "untrusted";

  return (
    <footer
      data-testid="status-bar"
      aria-label={t("status.label")}
      className="flex h-6 shrink-0 items-center gap-4 border-t border-stroke bg-sidebar px-3 text-caption text-fg-secondary"
    >
      <span className="inline-flex items-center gap-2" data-testid="status-connection">
        <span
          aria-hidden="true"
          data-tone={DOT[status]}
          className={`size-2 rounded-full ${TONE_CLASS[DOT[status]]}`}
        />
        <span>{t(`status.connection.${status}`)}</span>
      </span>
      <span
        className="tabular-nums"
        title={t("status.resourcesHint")}
        data-testid="status-resources"
      >
        {t("status.cpu", { value: totals === null ? dash : fmt.percent(totals.cpu / 100, 1) })}
        {SEPARATOR}
        {t("status.ram", { value: totals === null ? dash : fmt.bytes(totals.rss) })}
      </span>
      <Link to={DIAGNOSTICS} className={CHIP} data-testid={DNS_TESTID} data-state={dnsKey}>
        {t(`status.dns.${dnsKey}`)}
      </Link>
      <Link to={DIAGNOSTICS} className={CHIP} data-testid={CA_TESTID} data-state={caKey}>
        {t(`status.ca.${caKey}`)}
      </Link>
    </footer>
  );
}
