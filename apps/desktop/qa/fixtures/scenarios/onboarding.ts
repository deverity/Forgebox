// Handlers of the onboarding wizard (M2-E2-T19): the stack catalog of `catalog.ts` with a
// download that really finishes (the catalog fixture holds `redis` for its cancel flow) and the
// jobs of step 4. Options script the variants the acceptance criteria need:
// - `uac`: `net.dns_enable` fails with `needs_elevation` (a refused UAC);
// - `fail`: `tools.install` of `mysql` fails once (verification), then works;
// - `hosts`: `net.dns_enable` ends in the hosts fallback with its reason.
// Every call is recorded by the fake bridge, so tests read the order from `fake.calls`.
import { fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import { catalogHandlers } from "./catalog";
import type { QaData } from "./data";

const MB = 1024 * 1024;

export interface OnboardingQaOptions {
  uac?: boolean;
  fail?: boolean;
  hosts?: boolean;
}

export interface OnboardingQa {
  handlers: FakeHandlers;
  /** Specs installed so far. */
  installed: Set<string>;
}

const DNS_OK: T.DnsStatusDto = {
  enabled: true,
  server_running: true,
  listen: "127.0.0.1:53",
  tlds: [".test"],
  policy: "applied",
  mode: "nrpt",
};

export function createOnboardingQa(data: QaData, options: OnboardingQaOptions = {}): OnboardingQa {
  const catalog = catalogHandlers(data);
  const installed = new Set<string>();
  let failedOnce = false;

  const handlers: FakeHandlers = {
    ...catalog,
    "tools.plan": (params, ctx) => {
      // The catalog fixture answers a job whose outcome is the plan.
      const base = catalog["tools.plan"]?.(params, ctx) as
        { outcome: { done: T.ToolPlanDto } } | undefined;
      const plan: T.ToolPlanDto = base?.outcome.done ?? { items: [], total_bytes: 0 };
      const items = plan.items.map((item) => ({
        ...item,
        already_installed: installed.has(item.spec),
      }));
      return fakeJob({
        outcome: {
          done: {
            ...plan,
            items,
            total_bytes: items
              .filter((item) => !item.already_installed)
              .reduce((sum, item) => sum + (item.size_bytes ?? 0), 0),
          },
        },
      });
    },
    "tools.install": ({ specs }) => {
      if (options.fail === true && !failedOnce && specs.some((s) => s.startsWith("mysql"))) {
        failedOnce = true;
        return fakeJob({
          stepMs: 60,
          steps: [{ message: "downloading", pct: 40, bytes_done: 4 * MB, bytes_total: 10 * MB }],
          outcome: {
            failed: {
              code: "verification_failed",
              message: "sha256 mismatch for mysql",
              details: { reason: "sha256_mismatch", args: { tool: "mysql" } },
            },
          },
        });
      }
      for (const spec of specs) installed.add(spec);
      return fakeJob({
        stepMs: 60,
        steps: [
          { message: "downloading", pct: 50, bytes_done: 5 * MB, bytes_total: 10 * MB },
          { message: "extracting" },
        ],
        outcome: { done: { installed: [] } },
      });
    },
    "system.config_set": ({ key, value }) => ({ key, value, is_default: false }),
    "sites.park": ({ dir, tld }) => ({ dir, tld: tld ?? ".test", sites: 0 }),
    "shell.path_add": () => ({
      entries: ["C:/Forgebox/bin/php/current"],
      forgebox_entries: ["C:/Forgebox/bin/php/current"],
      missing: [],
    }),
    "net.dns_enable": () => {
      if (options.uac === true) {
        return fakeJob({
          stepMs: 60,
          outcome: {
            failed: {
              code: "needs_elevation",
              message: "the administrator confirmation was refused",
              details: { reason: "needs_elevation", args: { detail: "nrpt.add" } },
            },
          },
        });
      }
      return fakeJob({
        stepMs: 60,
        outcome: {
          done:
            options.hosts === true
              ? {
                  ...DNS_OK,
                  policy: "hosts_fallback",
                  mode: "hosts",
                  reason: "group_policy_table_present",
                }
              : DNS_OK,
        },
      });
    },
    "tls.ca_install": () =>
      fakeJob({
        stepMs: 60,
        outcome: {
          done: {
            exists: true,
            subject: "Forgebox Local CA",
            thumbprint_sha256: "AB",
            not_after: "2036-01-01T00:00:00Z",
            trusted_current_user: true,
            trusted_local_machine: false,
            name_constraints: [".test"],
          },
        },
      }),
  };
  return { handlers, installed };
}
