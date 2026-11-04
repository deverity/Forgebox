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
import { phpHandlers } from "./php";
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
      create(ctx, (params.host ?? "").toLowerCase(), {
        root: params.dir.replaceAll("\\", "/"),
        secure: params.secure ?? true,
        url: `${params.secure === false ? "http" : "https"}://${(params.host ?? "").toLowerCase()}`,
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

/** A scenario whose snapshot handlers answer from `data`. */
export function withData(name: string, data: QaData, extra: FakeScenario = {}): FakeScenario {
  const config = configHandlers(data);
  const detail = siteDetailHandlers(data);
  const catalog = catalogHandlers(data, config);
  const terminal = terminalHandlers();
  const php = phpHandlers(data, config);
  const databases = databasesHandlers();
  const answered = { ...detail, ...catalog, ...terminal, ...php, ...databases };
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
      ...php,
      ...databases,
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
  define("heavy", heavyData(500, 50), {}, eventStorm(50, 10));
  define("slow", populatedData(), builtin("slow"));
  define("remote", populatedData(), builtin("remote"));
}

/** The data and live behaviour of a registered scenario (empty data for the built-ins). */
export function liveBehaviourOf(name: string): { live: LiveBehaviour | undefined; data: QaData } {
  return { live: LIVE.get(name), data: DATA.get(name) ?? emptyData() };
}
