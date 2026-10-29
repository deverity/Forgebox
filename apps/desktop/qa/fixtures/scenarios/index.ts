// The QA scenario library (`?scenario=` in qa-mode builds), typed over fakeBridge. Handlers
// are checked against generated.ts through FakeScenario, so a DTO change breaks tsc.
// fakeBridge's SCENARIOS already holds daemon-down / not-implemented / errors; this module
// registers `empty`, `populated`, `heavy`, `slow` and `remote` with snapshot data and the
// live behaviour of `heavy` (event storm).
import {
  SCENARIOS,
  registerScenario,
  type FakeBridge,
  type FakeScenario,
} from "../../../src/ipc/fakeBridge";
import {
  CA_STATUS,
  DNS_STATUS,
  emptyData,
  heavyData,
  populatedData,
  servicePayload,
  type QaData,
} from "./data";

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

/** A scenario whose snapshot handlers answer from `data`. */
export function withData(name: string, data: QaData, extra: FakeScenario = {}): FakeScenario {
  return {
    ...extra,
    name,
    handlers: {
      "services.status": () => ({ instances: data.instances, pools: [] }),
      "sites.list": () => ({ sites: data.sites }),
      "tools.list": () => ({ tools: data.tools }),
      "net.dns_status": () => DNS_STATUS,
      "tls.ca_status": () => CA_STATUS,
      "daemon.status": () => data.daemon,
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
  define("populated", populatedData(), { latencyMs: 0 });
  define("heavy", heavyData(500, 50), {}, eventStorm(50, 10));
  define("slow", populatedData(), builtin("slow"));
  define("remote", populatedData(), builtin("remote"));
}

/** The data and live behaviour of a registered scenario (empty data for the built-ins). */
export function liveBehaviourOf(name: string): { live: LiveBehaviour | undefined; data: QaData } {
  return { live: LIVE.get(name), data: DATA.get(name) ?? emptyData() };
}
