// Handlers of the Diagnostics screen (M2-E3-T19): `daemon.doctor` with progress steps and a
// filter by id prefix (like the daemon's `wanted`), the fixes it names (`net.dns_enable`,
// `tls.ca_install`, `shell.path_add`) that really change the next answer, `net.port_check` (only for the held
// port; the scenario composes it with the config one) and `services.kill_port_holder`. State lives in the closure (one per scenario).
//
// The checks cover every case of the screen: a DNS policy and a CA with allowlisted fixes, a
// PATH one, a port conflict (80) with its holder, a free port, and a check whose `fix_method`
// is NOT in the client allowlist (`service.nginx` suggests `daemon.stop`): only "Copiar" there.
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";

const ACK: T.Ack = { ok: true, message: null };
const STEP_MS = 120;
/** The port the Diagnostics scenario has a foreign process on (`net.port_check` answers only it). */
export const QA_HOLDER_PORT = 80;
export const QA_HOLDER: T.PortHolderDto = { pid: 4412, exe: "httpd.exe (C:/xampp/apache/bin)" };

export interface DiagnosticsQa {
  handlers: FakeHandlers;
  /** What the fixes changed so far (tests read it). */
  state: { dns: boolean; ca: boolean; path: boolean; portFree: boolean };
}

function check(
  id: string,
  status: T.CheckStatus,
  message: string,
  patch: Partial<T.CheckDto> = {},
) {
  return { id, status, message, fix: null, ...patch } satisfies T.CheckDto;
}

export function createDiagnosticsQa(): DiagnosticsQa {
  const state = { dns: false, ca: false, path: false, portFree: false };
  const checks = (): T.CheckDto[] => [
    check("root.writable", "ok", "C:/Forgebox is writable"),
    check("store.schema", "ok", "schema v5"),
    check("dns.server", "ok", "DNS server on 127.0.0.1:53"),
    state.dns
      ? check("dns.policy.test", "ok", ".test routed by NRPT")
      : check("dns.policy.test", "warn", ".test is not routed (not_applied)", {
          fix: "forge dns enable",
          fix_method: "net.dns_enable",
        }),
    state.ca
      ? check("tls.ca_trusted", "ok", "trusted by Windows")
      : check("tls.ca_trusted", "warn", "not trusted by Windows", {
          fix: "forge tls ca install",
          fix_method: "tls.ca_install",
          fix_params: { scope: "current_user" },
        }),
    state.portFree
      ? check("port.80", "ok", "80 free or ours")
      : check("port.80", "warn", `80 in use by PID ${String(QA_HOLDER.pid)} (${QA_HOLDER.exe})`, {
          fix: "forge service ports",
        }),
    check("port.443", "ok", "443 free or ours"),
    state.path
      ? check("path.user", "ok", "3 Forgebox entries in the user PATH")
      : check("path.user", "warn", "3 Forgebox entries missing from the user PATH", {
          fix: "forge path add",
          fix_method: "shell.path_add",
        }),
    check("defender.exclusion", "skipped", "needs an administrator"),
    check("service.nginx", "fail", "nginx crashed 3 times", {
      fix: "forge daemon stop",
      fix_method: "daemon.stop",
    }),
  ];
  const wanted = (filters: readonly string[] | undefined, id: string) =>
    filters === undefined || filters.length === 0 || filters.some((f) => id.startsWith(f));
  const handlers: FakeHandlers = {
    "daemon.doctor": (params) =>
      fakeJob({
        stepMs: STEP_MS,
        steps: [
          { pct: 20, message: "dns" },
          { pct: 60, message: "ports" },
          { pct: 90, message: "versions" },
        ],
        outcome: { done: { checks: checks().filter((c) => wanted(params.checks, c.id)) } },
      }),
    "net.dns_enable": () => {
      state.dns = true;
      return fakeJob({
        stepMs: STEP_MS,
        outcome: {
          done: {
            enabled: true,
            server_running: true,
            listen: "127.0.0.1:53",
            tlds: ["test"],
            policy: "applied",
          } satisfies T.DnsStatusDto,
        },
      });
    },
    "tls.ca_install": () => {
      state.ca = true;
      return fakeJob({
        stepMs: STEP_MS,
        outcome: {
          done: {
            exists: true,
            subject: "Forgebox Local CA",
            thumbprint_sha256: "AB",
            not_after: "2036-01-01T00:00:00Z",
            trusted_current_user: true,
            trusted_local_machine: false,
            name_constraints: ["test"],
          } satisfies T.CaStatusDto,
        },
      });
    },
    "shell.path_add": () => {
      state.path = true;
      return { entries: [], forgebox_entries: [], missing: [] } satisfies T.UserPathDto;
    },
    "net.port_check": (params) => ({
      free: params.port !== QA_HOLDER_PORT || state.portFree,
      holder: params.port === QA_HOLDER_PORT && !state.portFree ? QA_HOLDER : null,
    }),
    "services.kill_port_holder": (params) => {
      if (params.confirm !== true) {
        fakeError("invalid_input", "kill_port_holder needs confirm: true", "confirm_required");
      }
      if (params.port !== 80 || params.pid !== QA_HOLDER.pid || state.portFree) {
        fakeError("conflict", "that process does not hold the port", "not_the_holder");
      }
      state.portFree = true;
      return ACK;
    },
    "logs.read": (params) => ({
      target: params.target,
      lines: [
        { seq: 1, ts: null, source: "stdout", level: "info", text: "forgeboxd started" },
        { seq: 2, ts: null, source: "stdout", level: "warn", text: "port 80 busy" },
      ],
      last_seq: 2,
    }),
  };
  return { handlers, state };
}

/** The scenario's handlers (a fresh state per call). */
export function diagnosticsHandlers(): FakeHandlers {
  return createDiagnosticsQa().handlers;
}
