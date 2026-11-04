// Handlers of Settings > Rede and Seguranca (M2-E3-T17): `net.dns_*` (status, enable/disable
// jobs that need the UAC prompt, `dns_test`), the CA (`tls.ca_*` jobs, status, export), the
// certificate list with `tls.renew`, `services.ports` with an external conflict and `fs.tail`
// of the helper log. State lives in the closure; `elevation: "refused"` makes every job that
// would show a UAC prompt fail with `needs_elevation`, like a user who clicks "No".
//
// Registered as the `network-security` scenario (not part of the shared composition: the
// Diagnostics scenario answers `net.dns_enable` and `tls.ca_install` its own way).
import { fakeError, fakeJob, type FakeHandlers, type FakeJob } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";

const STEP_MS = 120;
const DAY_MS = 86_400_000;

export const QA_PEM =
  "-----BEGIN CERTIFICATE-----\nMIIBforgebox-local-ca\n-----END CERTIFICATE-----\n";

export interface NetworkSecurityState {
  dns: { enabled: boolean; fallback: boolean };
  ca: T.CaStatusDto;
  certificates: T.CertificateDto[];
  /** `refused`: jobs that elevate fail with `needs_elevation`. */
  elevation: "granted" | "refused";
  /** Methods called, with their params (tests assert on what the screen sent). */
  calls: { method: string; params: unknown }[];
}

export interface NetworkSecurityQa {
  handlers: FakeHandlers;
  state: NetworkSecurityState;
}

function certificate(host: string, daysFromNow: number, patch: Partial<T.CertificateDto> = {}) {
  const now = Date.now();
  return {
    host,
    sans: [host],
    site_id: null,
    not_before: new Date(now - 30 * DAY_MS).toISOString(),
    not_after: new Date(now + daysFromNow * DAY_MS).toISOString(),
    thumbprint_sha256: `CE${host.length.toString(16)}`,
    ca_thumbprint_sha256: "AB",
    trusted: true,
    ...patch,
  } satisfies T.CertificateDto;
}

const NEEDS_ELEVATION = {
  code: "needs_elevation",
  message: "the administrator prompt was refused",
  details: { reason: "uac_refused", args: { detail: "UAC was dismissed" } },
};

export function createNetworkSecurityQa(
  initial: { elevation?: "granted" | "refused"; fallback?: boolean } = {},
): NetworkSecurityQa {
  const state: NetworkSecurityState = {
    dns: { enabled: false, fallback: initial.fallback ?? false },
    ca: {
      exists: true,
      subject: "Forgebox Local CA",
      thumbprint_sha256: "AB12CD34EF56",
      not_after: new Date(Date.now() + 3000 * DAY_MS).toISOString(),
      trusted_current_user: true,
      trusted_local_machine: false,
      name_constraints: [".test", ".localhost"],
    },
    certificates: [
      certificate("loja.test", 10),
      certificate("blog.test", 200),
      certificate("velho.test", -3),
      certificate("antigo.test", 200, { trusted: false, ca_thumbprint_sha256: "OLD" }),
    ],
    elevation: initial.elevation ?? "granted",
    calls: [],
  };
  const record = (method: string, params: unknown) => {
    state.calls.push({ method, params });
  };
  const elevated = <R>(done: R, effect: () => void): FakeJob<R> =>
    state.elevation === "refused"
      ? fakeJob<R>({ stepMs: STEP_MS, outcome: { failed: NEEDS_ELEVATION } })
      : fakeJob<R>({
          stepMs: STEP_MS,
          steps: [{ pct: null, message: "waiting for the administrator prompt" }],
          outcome: { done },
          onFinish: (outcome) => {
            if ("done" in outcome) effect();
          },
        });
  const dnsStatus = (): T.DnsStatusDto => {
    const { enabled, fallback } = state.dns;
    if (!enabled) {
      return {
        enabled,
        server_running: false,
        listen: null,
        tlds: [".test"],
        policy: "not_applied",
      };
    }
    return {
      enabled,
      server_running: true,
      listen: "127.0.0.1:53",
      tlds: [".test"],
      policy: fallback ? "hosts_fallback" : "applied",
      mode: fallback ? "hosts" : "nrpt",
      reason: fallback ? "group_policy_table_present" : null,
      tld_policies: [
        {
          tld: ".test",
          mode: fallback ? "hosts" : "nrpt",
          policy: fallback ? "hosts_fallback" : "applied",
          nameserver: "127.0.0.1",
          reason: fallback ? "group_policy_table_present" : null,
          group_policy_table: fallback,
          probe: null,
        },
      ],
    };
  };
  const handlers: FakeHandlers = {
    "net.dns_status": () => dnsStatus(),
    "net.dns_enable": (params) => {
      record("net.dns_enable", params);
      return elevated(dnsStatus(), () => {
        state.dns.enabled = true;
      });
    },
    "net.dns_disable": (params) => {
      record("net.dns_disable", params);
      return elevated(dnsStatus(), () => {
        state.dns.enabled = false;
      });
    },
    "net.dns_test": (params) => {
      record("net.dns_test", params);
      const ok = state.dns.enabled;
      return {
        host: params.host,
        resolved: ok ? ["127.0.0.1"] : ["203.0.113.9"],
        expected: ["127.0.0.1"],
        ok,
        via: ok ? "forgebox-dns" : "system",
        mode: ok ? (state.dns.fallback ? "hosts" : "nrpt") : "none",
        reason: ok && state.dns.fallback ? "group_policy_table_present" : null,
      } satisfies T.DnsTestDto;
    },
    "tls.ca_status": () => state.ca,
    "tls.ca_install": (params) => {
      record("tls.ca_install", params);
      const machine = params.scope === "local_machine";
      const apply = () => {
        if (machine) state.ca.trusted_local_machine = true;
        else state.ca.trusted_current_user = true;
      };
      return machine
        ? elevated(state.ca, apply)
        : fakeJob({ stepMs: STEP_MS, outcome: { done: state.ca }, onFinish: apply });
    },
    "tls.ca_uninstall": (params) => {
      record("tls.ca_uninstall", params);
      const machine = params.scope === "local_machine";
      const apply = () => {
        if (machine) state.ca.trusted_local_machine = false;
        else state.ca.trusted_current_user = false;
      };
      return machine
        ? elevated(state.ca, apply)
        : fakeJob({ stepMs: STEP_MS, outcome: { done: state.ca }, onFinish: apply });
    },
    "tls.ca_export": (params) => {
      record("tls.ca_export", params);
      return { filename: "forgebox-ca.crt", pem: QA_PEM } satisfies T.PemDto;
    },
    "tls.ca_rotate": (params) => {
      record("tls.ca_rotate", params);
      if (params.confirm !== true) {
        fakeError("invalid_input", "tls.ca_rotate needs confirm: true", "confirm_required");
      }
      return fakeJob({
        stepMs: STEP_MS,
        steps: [
          { pct: 30, message: "creating the new CA" },
          { pct: 80, message: "reissuing certificates" },
        ],
        outcome: { done: state.ca },
        onFinish: (outcome) => {
          if ("done" in outcome) {
            state.ca.thumbprint_sha256 = "NEW9988776655";
            for (const cert of state.certificates) cert.ca_thumbprint_sha256 = "NEW";
          }
        },
      });
    },
    "tls.list": () => ({ certificates: state.certificates }),
    "tls.renew": (params) => {
      record("tls.renew", params);
      return fakeJob({
        stepMs: STEP_MS,
        outcome: { done: { certificates: state.certificates } },
        onFinish: (outcome) => {
          if (!("done" in outcome)) return;
          for (const cert of state.certificates) {
            if (params.all === true || cert.host === params.host) {
              cert.not_after = new Date(Date.now() + 365 * DAY_MS).toISOString();
              cert.trusted = true;
            }
          }
        },
      });
    },
    "services.ports": () => ({
      ports: [
        {
          id: "01QAPORT0000000000000000A1",
          port: 80,
          protocol: "tcp",
          owner_kind: "instance",
          owner_id: "01QAINST0000000000000000A1",
          owner_name: "nginx",
          name: "http",
          state: "bound",
          conflict: null,
        },
        {
          id: "01QAPORT0000000000000000A2",
          port: 3306,
          protocol: "tcp",
          owner_kind: "instance",
          owner_id: "01QAINST0000000000000000A2",
          owner_name: "mysql/default",
          name: "main",
          state: "reserved",
          conflict: null,
        },
      ],
      external: [
        {
          port: 443,
          protocol: "tcp",
          holder: { pid: 4412, exe: "httpd.exe (C:/xampp/apache/bin)" },
        },
      ],
    }),
    "fs.tail": (params) => {
      record("fs.tail", params);
      return {
        path: params.path,
        size: 240,
        lines: [
          JSON.stringify({
            ts: "2026-11-01T10:00:00Z",
            request_id: "r1",
            op: "dns.nrpt_apply",
            dry_run: false,
            ok: true,
            code: "ok",
            message: "NRPT rule applied for .test",
          }),
          JSON.stringify({
            ts: "2026-11-01T10:05:00Z",
            request_id: "r2",
            op: "trust.install_machine_root",
            dry_run: false,
            ok: false,
            code: "needs_elevation",
            message: "the helper was not elevated",
          }),
        ],
      } satisfies T.FsTailDto;
    },
  };
  return { handlers, state };
}
