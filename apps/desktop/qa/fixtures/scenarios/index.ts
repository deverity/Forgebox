// The QA scenario library (`?scenario=` in qa-mode builds), typed over fakeBridge. Handlers
// are checked against generated.ts through FakeScenario, so a DTO change breaks tsc.
// fakeBridge's SCENARIOS already holds daemon-down / not-implemented / errors; this module
// registers `empty`, `populated`, `heavy`, `slow` and `remote` with snapshot data and the
// live behaviour of `heavy` (event storm).
import {
  NOT_IMPLEMENTED,
  SCENARIOS,
  fakeError,
  fakeJob,
  registerScenario,
  type FakeBridge,
  type FakeHandlers,
  type FakeScenario,
} from "../../../src/ipc/fakeBridge";
import { catalogHandlers } from "./catalog";
import { attentionDashboard, healthyDashboard } from "./dashboard";
import { configHandlers } from "./config";
import { databasesHandlers } from "./databases";
import { createFilesystemQa } from "./filesystem";
import { diagnosticsHandlers, QA_HOLDER_PORT } from "./diagnostics";
import {
  CA_STATUS,
  DNS_STATUS,
  emptyData,
  heavyData,
  populatedData,
  servicePayload,
  site,
  type QaData,
} from "./data";
import { logsHandlers } from "./logs";
import { metricsHandlers } from "./metrics";
import { mailHandlers } from "./mail";
import { createOnboardingQa } from "./onboarding";
import { phpHandlers } from "./php";
import { createNetworkSecurityQa } from "./networkSecurity";
import { settingsHandlers } from "./settings";
import { siteDetailHandlers } from "./siteDetail";
import { terminalHandlers } from "./terminal";

export const QA_SCENARIO_NAMES = [
  "empty",
  "populated",
  "heavy",
  "errors",
  "not-implemented",
  "daemon-down",
  "slow",
  "remote",
] as const;
export type QaScenarioName = (typeof QA_SCENARIO_NAMES)[number];

const QA_TLD = ".test";

/** `<folder>.<tld>`, like the daemon when `host` is absent. */
function defaultHost(dir: string): string {
  const folder = dir.replaceAll("\\", "/").replace(/\/+$/, "").split("/").pop() ?? "site";
  return `${folder.toLowerCase().replaceAll(/[^a-z0-9-]+/g, "-")}${QA_TLD}`;
}

/**
 * `sites.add` / `sites.proxy_add` / `sites.remove` over `data.sites`, publishing the `site.*`
 * events the daemon would (M2-E2-T15). A host outside `.test` is an `invalid_input` on `host`,
 * like the daemon's TLD rule (docs/06 section 1.3).
 */
export function siteHandlers(data: QaData): FakeHandlers {
  let next = 10_000;
  const create = (ctx: { fake: FakeBridge }, host: string, patch: Parameters<typeof site>[2]) => {
    if (!host.endsWith(QA_TLD)) {
      fakeError("invalid_input", `host ${host} is not under a configured TLD`, "host_tld", {
        field: "host",
        host,
        tlds: [QA_TLD],
      });
    }
    if (data.sites.some((existing) => existing.host === host)) {
      fakeError("conflict", `host ${host} is already used`, "host_in_use", { host });
    }
    next += 1;
    const created = site(next, host, patch);
    data.sites.push(created);
    ctx.fake.publish("site.created", { v: 1, site_id: created.id, host });
    return fakeJob({ outcome: { done: created } });
  };
  return {
    "sites.add": (params, ctx) =>
      create(ctx, (params.host ?? defaultHost(params.dir)).toLowerCase(), {
        root: params.dir.replaceAll("\\", "/"),
        secure: params.secure ?? true,
        url: `${params.secure === false ? "http" : "https"}://${(params.host ?? defaultHost(params.dir)).toLowerCase()}`,
        driver: params.driver ?? "generic-php",
        server: params.server ?? "nginx",
      }),
    "sites.proxy_add": (params, ctx) =>
      create(ctx, params.host.toLowerCase(), {
        kind: "proxy",
        root: null,
        php: null,
        upstream: params.upstream,
        websocket: params.websocket ?? false,
        secure: params.secure ?? true,
        url: `https://${params.host.toLowerCase()}`,
      }),
    "sites.remove": (params, ctx) => {
      if (params.confirm !== true) {
        fakeError("invalid_input", "sites.remove needs confirm: true", "confirm_required");
      }
      const found = data.sites.find((s) => s.id === params.site || s.host === params.site);
      if (found === undefined) fakeError("not_found", `site ${params.site} not found`);
      data.sites.splice(data.sites.indexOf(found), 1);
      ctx.fake.publish("site.removed", { v: 1, site_id: found.id, host: found.host });
      return fakeJob({ outcome: { done: { ok: true, message: null } } });
    },
  };
}

/** `{specs: ["php@8.4"]}`: one bare PHP series, no variant, stack or target. */
function phpSeriesOnly(params: {
  specs?: readonly string[];
  stack?: string | null;
  variant?: string | null;
  target?: string | null;
}): boolean {
  const specs = params.specs ?? [];
  return (
    specs.length === 1 &&
    /^php@\d+\.\d+$/.test(specs[0] ?? "") &&
    (params.stack ?? null) === null &&
    (params.variant ?? null) === null &&
    (params.target ?? null) === null
  );
}

/** A scenario whose snapshot handlers answer from `data`. */
export function withData(name: string, data: QaData, extra: FakeScenario = {}): FakeScenario {
  const config = configHandlers(data);
  const detail = siteDetailHandlers(data);
  const catalog = catalogHandlers(data, config);
  const terminal = terminalHandlers();
  const logs = logsHandlers();
  const mail = mailHandlers(data).handlers;
  const php = phpHandlers(data, config);
  const databases = databasesHandlers();
  const diagnostics = diagnosticsHandlers();
  const settings = settingsHandlers().handlers;
  const metrics = metricsHandlers(data);
  const answered = {
    ...metrics.handlers,
    ...detail,
    ...catalog,
    ...terminal,
    ...logs,
    ...mail,
    ...php,
    ...databases,
    ...diagnostics,
    ...settings,
  };
  return {
    // The Site detail and the catalog answer `tools.plan` etc. before their backend tasks land.
    notImplemented: NOT_IMPLEMENTED.map((row) => row.method).filter(
      (method) => !(method in answered),
    ),
    ...extra,
    name,
    handlers: {
      "services.status": () => ({ instances: data.instances, pools: [] }),
      "sites.list": () => ({ sites: data.sites }),
      "tools.list": () => ({ tools: data.tools }),
      "net.dns_status": () => DNS_STATUS,
      // The Dashboard (M2-E2-T14) asks for both: nothing external, no certificates by default.
      "services.ports": () => ({ ports: [], external: [] }),
      "tls.list": () => ({ certificates: [] }),
      "tls.ca_status": () => CA_STATUS,
      "daemon.status": () => data.daemon,
      ...siteHandlers(data),
      ...detail,
      ...config,
      ...catalog,
      ...terminal,
      ...logs,
      ...mail,
      ...php,
      ...databases,
      ...diagnostics,
      ...settings,
      ...metrics.handlers,
      // `metrics.enabled` is the daemon's global switch (M2-E5-T16); the rest is Settings'.
      "system.config_get": (params, ctx) => {
        if (params.key === "metrics.enabled") {
          return { key: params.key, value: metrics.globalEnabled(), is_default: true };
        }
        const handler = settings["system.config_get"];
        if (handler === undefined) fakeError("not_found", `unknown key ${params.key}`);
        return handler(params, ctx);
      },
      "system.config_set": (params, ctx) => {
        if (params.key === "metrics.enabled") {
          metrics.setGlobalEnabled(params.value === true);
          ctx.fake.publish("config.changed", { v: 1, keys: [params.key], requires: [] });
          return { key: params.key, value: params.value, is_default: false, requires: [] };
        }
        const handler = settings["system.config_set"];
        if (handler === undefined) fakeError("not_found", `unknown key ${params.key}`);
        return handler(params, ctx);
      },
      // `logs.read` without `before_seq` is the Diagnostics export of the daemon log; paging back
      // (the Logs screen) reads the history of `logs.ts`.
      "logs.read": (params, ctx) => {
        const handler =
          params.before_seq === undefined || params.before_seq === null
            ? diagnostics["logs.read"]
            : logs["logs.read"];
        if (handler === undefined) return { target: params.target, lines: [], last_seq: 0 };
        return handler(params, ctx);
      },
      // Both answer `net.port_check`: the held port is the Diagnostics scenario's, the rest config's.
      "net.port_check": (params, ctx) => {
        const handler =
          params.port === QA_HOLDER_PORT ? diagnostics["net.port_check"] : config["net.port_check"];
        if (handler === undefined) return { free: true };
        return handler(params, ctx);
      },
      // Both answer `tools.plan`/`tools.install`: a lone PHP series without variant (the Site
      // detail's "install and switch", `php@7.4`) goes to siteDetail.ts, which also marks it
      // installed for the switch; exact versions, variants and stacks (the catalog) to catalog.ts.
      "tools.plan": (params, ctx) => {
        const handler = phpSeriesOnly(params) ? detail["tools.plan"] : catalog["tools.plan"];
        if (handler === undefined) fakeError("not_implemented", "tools.plan");
        return handler(params, ctx);
      },
      "tools.install": (params, ctx) => {
        const handler = phpSeriesOnly(params) ? detail["tools.install"] : catalog["tools.install"];
        if (handler === undefined) fakeError("not_implemented", "tools.install");
        return handler(params, ctx);
      },
      ...extra.handlers,
    },
  } satisfies FakeScenario;
}

/** Behaviour started once the bridge exists; returns its cleanup. */
export type LiveBehaviour = (fake: FakeBridge, data: QaData) => () => void;

/** Flips `perTick` instances between running and stopped every `everyMs` (the `heavy` storm). */
export const eventStorm =
  (everyMs: number, perTick: number): LiveBehaviour =>
  (fake, data) => {
    let tick = 0;
    const timer = setInterval(() => {
      for (let i = 0; i < perTick; i += 1) {
        const inst = data.instances[(tick * perTick + i) % Math.max(1, data.instances.length)];
        if (inst === undefined) return;
        const running = tick % 2 === 0;
        fake.publish(
          running ? "service.running" : "service.stopped",
          servicePayload(inst, running ? "running" : "stopped"),
        );
      }
      tick += 1;
    }, everyMs);
    return () => {
      clearInterval(timer);
    };
  };

const LIVE = new Map<string, LiveBehaviour>();
const DATA = new Map<string, QaData>();

function define(name: string, data: QaData, base: FakeScenario, live?: LiveBehaviour): void {
  DATA.set(name, data);
  registerScenario(name, () => withData(name, data, base));
  if (live) LIVE.set(name, live);
}

/** Registers the library into fakeBridge's SCENARIOS (idempotent). */
export function registerQaScenarios(): void {
  const builtin = (name: string): FakeScenario => SCENARIOS.get(name)?.() ?? {};
  define("empty", emptyData(), builtin("empty"));
  // The catalog (M2-E2-T18) uses the provider jobs (`tools.releases`, `tools.plan`, answered
  // since M2-E2-T5, so they are no longer in the not-implemented table).
  define("populated", populatedData(), {
    latencyMs: 0,
    notImplemented: NOT_IMPLEMENTED.map((row) => row.method),
  });
  // The Dashboard (M2-E2-T14): healthy, and every Atenção source at once.
  const healthy = healthyDashboard();
  define("dashboard", healthy.data, healthy.extra);
  const attention = attentionDashboard();
  define("dashboard-attention", attention.data, attention.extra);
  // The onboarding wizard (M2-E2-T19): all green, a refused UAC, and one failed download.
  const onboarding = (name: string, options: Parameters<typeof createOnboardingQa>[1]) => {
    const data = populatedData();
    define(name, data, {
      latencyMs: 0,
      notImplemented: [],
      handlers: createOnboardingQa(data, options).handlers,
    });
  };
  onboarding("onboarding", {});
  onboarding("onboarding-uac", { uac: true });
  onboarding("onboarding-fail", { fail: true });
  onboarding("onboarding-hosts", { hosts: true });
  // Settings > Rede and Seguranca (M2-E3-T17): DNS/CA jobs with the UAC prompt, certificates,
  // external port conflict, helper log. Its own scenario: Diagnostics answers `net.dns_enable`
  // and `tls.ca_install` differently.
  define("network-security", populatedData(), {
    latencyMs: 0,
    notImplemented: NOT_IMPLEMENTED.map((row) => row.method),
    handlers: createNetworkSecurityQa().handlers,
  });
  // Settings > Sistema de arquivos, Registries and Atualizacoes (M2-E3-T18).
  define("filesystem", populatedData(), {
    latencyMs: 0,
    notImplemented: NOT_IMPLEMENTED.map((row) => row.method),
    handlers: createFilesystemQa().handlers,
  });
  define("heavy", heavyData(500, 50), {}, eventStorm(50, 10));
  // Populated data with the site-tab methods failing (M2-E3-T21): Banco (`db.list`),
  // Ambiente (`sites.env`), Processos (`sites.ps`) and Desempenho (`stats.series`, M2-E5-T16) must show cause + action, not a blank tab.
  define("tab-errors", populatedData(), {
    latencyMs: 0,
    notImplemented: NOT_IMPLEMENTED.map((row) => row.method),
    handlers: Object.fromEntries(
      ["db.list", "sites.env", "sites.ps", "stats.series"].map((method) => [
        method,
        () =>
          fakeError("internal", `${method} failed (scenario tab-errors)`, "fake_error", { method }),
      ]),
    ),
  });
  define("slow", populatedData(), builtin("slow"));
  define("remote", populatedData(), builtin("remote"));
}

/** The data and live behaviour of a registered scenario (empty data for the built-ins). */
export function liveBehaviourOf(name: string): { live: LiveBehaviour | undefined; data: QaData } {
  return { live: LIVE.get(name), data: DATA.get(name) ?? emptyData() };
}
