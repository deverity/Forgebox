// Turns a daemon event into a sentence for the activity list ("PHP 8.3.37 instalado · há 2 min",
// docs/07 section 5.2). Pure: it returns an i18n key of the `dashboard` namespace plus its
// values, so the sentence follows the UI language; the relative time is added by the caller.
// Events that are noise for a person (job progress, metrics, ...) are not humanized (`null`).
import type * as T from "../../../ipc/generated";

export type ActivityTone = "neutral" | "success" | "warning" | "danger";

export interface HumanEvent {
  /** Key under `activity.event.` in the `dashboard` namespace. */
  key: string;
  values: Record<string, string | number>;
  tone: ActivityTone;
}

export interface HumanizeOptions {
  /** Display name of a tool id (`php` -> `PHP`); the id itself when unknown. */
  toolName?: (id: string) => string;
}

type Payload = Record<string, unknown>;

function record(value: unknown): Payload {
  return typeof value === "object" && value !== null ? (value as Payload) : {};
}

function text(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : typeof value === "number" ? String(value) : fallback;
}

type Builder = (payload: Payload, tool: (id: string) => string) => Omit<HumanEvent, "key">;

const plain =
  (
    tone: ActivityTone,
    values: (p: Payload, tool: (id: string) => string) => HumanEvent["values"],
  ): Builder =>
  (p, tool) => ({ tone, values: values(p, tool) });

const BUILDERS: Readonly<Record<string, Builder>> = {
  "tool.install.completed": plain("success", (p, tool) => ({
    tool: tool(text(p.tool)),
    version: text(p.version),
  })),
  "tool.install.failed": plain("danger", (p, tool) => ({
    tool: tool(text(p.tool)),
    version: text(p.version),
  })),
  "tool.updated": plain("success", (p, tool) => ({ tool: tool(text(p.tool)), to: text(p.to) })),
  "tool.rollback": plain("warning", (p, tool) => ({ tool: tool(text(p.tool)), to: text(p.to) })),
  "service.running": plain("success", (p) => ({ name: text(p.name) })),
  "service.stopped": plain("neutral", (p) => ({ name: text(p.name) })),
  "service.failed": plain("danger", (p) => ({ name: text(p.name) })),
  "service.crashed": plain("danger", (p) => ({ name: text(p.name) })),
  "site.created": plain("success", (p) => ({ host: text(p.host) })),
  "site.removed": plain("neutral", (p) => ({ host: text(p.host) })),
  "site.php_changed": plain("neutral", (p) => ({ host: text(p.host), to: text(p.to) })),
  "tls.leaf_issued": plain("success", (p) => ({ host: text(p.host) })),
  "tls.renewed": plain("success", (p) => ({ host: text(p.host) })),
  "tls.ca_created": plain("neutral", () => ({})),
  "tls.ca_untrusted": plain("warning", () => ({})),
  "dns.policy_applied": plain("neutral", (p) => ({ tld: text(p.tld), policy: text(p.policy) })),
  "config.drift_detected": plain("warning", (p) => ({ path: text(p.path) })),
  "port.conflict": plain("warning", (p) => ({ port: text(p.port) })),
  "tool.updates_available": plain("neutral", (p) => ({ count: Number(p.count ?? 0) })),
  "daemon.ready": plain("neutral", () => ({})),
};

/** The kinds worth asking `events.list` for (the filter of the activity history). */
export const ACTIVITY_KINDS: readonly string[] = Object.keys(BUILDERS);

export function humanizeEvent(
  event: Pick<T.EventDto, "kind" | "payload">,
  options: HumanizeOptions = {},
): HumanEvent | null {
  const build = BUILDERS[event.kind];
  if (build === undefined) return null;
  const tool = options.toolName ?? ((id: string) => id);
  const built = build(record(event.payload), tool);
  return { key: event.kind.replaceAll(".", "_"), ...built };
}
