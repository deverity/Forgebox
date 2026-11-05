// Handlers of the Settings screen (M2-E3-T16): `system.config_list|config_get|config_set` over an
// in-memory `forgebox.toml`, `shell.open_editor` and `shell.open`. The schema has the shape
// schemars gives `ForgeConfig` (`$defs`, `$ref`, `type: [X, "null"]`), trimmed to the keys the
// screens use. `config_set` validates like the daemon (`config_invalid`), answers `requires`
// like `config_owner::requires_for` and publishes `config.changed`.
import { fakeError, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";

const ACK: T.Ack = { ok: true, message: null };

/** The JSON Schema of the configuration (subset). */
export const QA_CONFIG_SCHEMA = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  title: "ForgeConfig",
  type: "object",
  properties: {
    tld: { description: "TLD of local sites, with the leading dot.", type: "string" },
    case_sensitive: { description: "Case-sensitive directories by default.", type: "boolean" },
    parked_dirs: { type: "array", items: { type: "string" } },
    ports: { $ref: "#/$defs/PortsConfig" },
    dns: { $ref: "#/$defs/DnsConfig" },
    updates: { $ref: "#/$defs/UpdatesConfig" },
    editor: { $ref: "#/$defs/EditorConfig" },
    terminal: { $ref: "#/$defs/TerminalConfig" },
  },
  $defs: {
    PortsConfig: {
      type: "object",
      properties: {
        reserved: { type: "array", items: { type: "integer", format: "uint16", minimum: 0 } },
        ranges: { type: "object", additionalProperties: { type: "array" } },
      },
    },
    DnsConfig: {
      type: "object",
      properties: {
        enabled: { description: "Local DNS resolver.", type: "boolean" },
        extra_tlds: { type: "array", items: { type: "string" } },
      },
    },
    MeteredPolicy: {
      description: "What to do on a metered connection.",
      oneOf: [
        { type: "string", const: "pause" },
        { type: "string", const: "allow" },
      ],
    },
    QuietHours: {
      type: "object",
      properties: { start: { type: "string" }, end: { type: "string" } },
    },
    UpdatesConfig: {
      type: "object",
      properties: {
        metered: { $ref: "#/$defs/MeteredPolicy" },
        quiet_hours: {
          description: "Window in which background updates do not run.",
          anyOf: [{ $ref: "#/$defs/QuietHours" }, { type: "null" }],
        },
      },
    },
    EditorConfig: {
      type: "object",
      properties: { command: { type: ["string", "null"] } },
    },
    TerminalConfig: {
      type: "object",
      properties: {
        command: { type: ["string", "null"] },
        integrated_shell: { type: ["string", "null"] },
        max_sessions: { type: "integer", format: "uint32", minimum: 1, maximum: 256 },
      },
    },
  },
} as const;

const DEFAULT_VALUES = {
  tld: ".test",
  case_sensitive: true,
  parked_dirs: ["www"],
  ports: {
    reserved: [9003],
    ranges: {
      web: [80, 443],
      "web-alt": [8080, 8443],
      databases: ["3306-3399", "5432-5499"],
    } as Record<string, (number | string)[]>,
  },
  dns: { enabled: true, extra_tlds: [] as string[] },
  updates: {
    metered: "pause",
    quiet_hours: null as { start: string; end: string } | null,
  },
  editor: { command: null as string | null },
  terminal: {
    command: null as string | null,
    integrated_shell: null as string | null,
    max_sessions: 16,
  },
};

type Values = typeof DEFAULT_VALUES;

/** A write the fake daemon refuses (the real one validates the whole document). */
export const QA_INVALID_EDITOR = "BAD";

function clone<V>(value: V): V {
  return JSON.parse(JSON.stringify(value)) as V;
}

function read(values: unknown, key: string): unknown {
  let current: unknown = values;
  for (const part of key.split(".")) {
    if (typeof current !== "object" || current === null) return undefined;
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

function write(values: Values, key: string, value: unknown): void {
  const parts = key.split(".");
  const last = parts.pop();
  let current: Record<string, unknown> = values;
  for (const part of parts) {
    const next = current[part];
    if (typeof next !== "object" || next === null) fakeError("not_found", `unknown key ${key}`);
    current = next as Record<string, unknown>;
  }
  if (last === undefined || !(last in current)) fakeError("not_found", `unknown key ${key}`);
  current[last] = value;
}

export interface SettingsFake {
  handlers: FakeHandlers;
  /** The current document (tests assert on it). */
  values(): Values;
}

/** Handlers over a fresh configuration; `values` lets a test change it as another client would. */
export function settingsHandlers(initial: Partial<Values> = {}): SettingsFake {
  const values: Values = { ...clone(DEFAULT_VALUES), ...clone(initial) };
  const defaults = clone(DEFAULT_VALUES);
  const keys = [
    "tld",
    "case_sensitive",
    "parked_dirs",
    "ports.ranges.web",
    "ports.ranges.web-alt",
    "ports.ranges.databases",
    "dns.enabled",
    "dns.extra_tlds",
    "updates.metered",
    "updates.quiet_hours",
    "editor.command",
    "terminal.command",
    "terminal.integrated_shell",
    "terminal.max_sessions",
  ];
  const document = (): T.ConfigDocumentDto => ({
    values: clone(values),
    defaults,
    schema: QA_CONFIG_SCHEMA,
    set_keys: keys.filter(
      (key) => JSON.stringify(read(values, key)) !== JSON.stringify(read(defaults, key)),
    ),
  });
  return {
    values: () => values,
    handlers: {
      "system.config_list": () => document(),
      "system.config_get": (params) => {
        const value = read(values, params.key);
        if (value === undefined) fakeError("not_found", `unknown key ${params.key}`);
        return {
          key: params.key,
          value,
          is_default: JSON.stringify(value) === JSON.stringify(read(defaults, params.key)),
        };
      },
      "system.config_set": (params, { fake }) => {
        if (read(values, params.key) === undefined) {
          fakeError("not_found", `unknown key ${params.key}`);
        }
        if (params.key === "editor.command" && params.value === QA_INVALID_EDITOR) {
          fakeError("config_invalid", "editor.command: unbalanced quotes", undefined, {
            key: params.key,
          });
        }
        if (params.key === "tld" && !/^\.[a-z]+$/.test(String(params.value))) {
          fakeError("config_invalid", `tld ${String(params.value)} must look like .test`);
        }
        write(values, params.key, params.value);
        const requires = params.key === "tld" ? ["tls.ca_rotate", "net.dns_enable"] : [];
        fake.publish("config.changed", { v: 1, keys: [params.key], requires });
        return {
          key: params.key,
          value: params.value,
          is_default: JSON.stringify(params.value) === JSON.stringify(read(defaults, params.key)),
          requires,
        };
      },
      "shell.open_editor": () => ACK,
      "shell.open": () => ACK,
    },
  };
}
