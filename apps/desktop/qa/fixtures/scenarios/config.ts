// Handlers of the Services detail tabs (M2-E2-T13): quick settings, config files, `fs.read`,
// `tools.info`/`tools.history` and the mutations behind them, over the QA data. Settings are
// kept per target so a `services.set` shows up in the next `services.settings`.
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import { fakeId, type QaData } from "./data";

/** A target that holds this port in the allocator (the conflict the form refuses). */
export const QA_RESERVED_PORT = 6379;
export const QA_RESERVED_BY = "redis/default";

/** One setting of each of the eight control kinds. */
function baseSettings(): T.QuickSettingDto[] {
  return [
    {
      key: "slow_query_log",
      label: "Slow query log",
      kind: "toggle",
      value: false,
      default: false,
      is_default: true,
      apply: "restart",
    },
    {
      key: "sql_mode",
      label: "SQL mode",
      kind: "select",
      options: ["strict", "traditional", "ansi"],
      value: "strict",
      default: "strict",
      is_default: true,
      apply: "restart",
    },
    {
      key: "max_connections",
      label: "Max connections",
      kind: "number",
      min: 10,
      max: 1000,
      value: 151,
      default: 151,
      is_default: true,
      apply: "restart",
    },
    {
      key: "server_comment",
      label: "Comment",
      kind: "text",
      value: "Forgebox",
      default: "Forgebox",
      is_default: true,
      apply: "none",
    },
    {
      key: "tmpdir",
      label: "Temp folder",
      kind: "path",
      value: "C:/Forgebox/tmp",
      default: "C:/Forgebox/tmp",
      is_default: true,
      apply: "restart",
    },
    {
      key: "debug_port",
      label: "Debug port",
      kind: "port",
      value: 9003,
      default: 9003,
      is_default: true,
      apply: "restart",
      reserve: true,
    },
    {
      key: "buffer_pool_mb",
      label: "Buffer pool (MB)",
      kind: "slider",
      min: 64,
      max: 2048,
      step: 64,
      value: 512,
      default: 512,
      is_default: true,
      apply: "restart",
    },
    {
      key: "plugins",
      label: "Plugins",
      kind: "multi",
      options: ["audit", "validate_password", "ngram"],
      value: ["audit"],
      default: ["audit"],
      is_default: true,
      apply: "reload",
    },
  ];
}

function textOf(value: T.SettingValueDto | null | undefined): string {
  if (value === null || value === undefined) return "";
  return Array.isArray(value) ? value.join(",") : String(value);
}

function typed(setting: T.QuickSettingDto, text: string): T.SettingValueDto {
  switch (setting.kind) {
    case "toggle":
      return text === "true";
    case "number":
    case "slider":
    case "port":
      return Number(text);
    case "multi":
      return text === "" ? [] : text.split(",");
    default:
      return text;
  }
}

const INI = [
  "[mysqld]",
  "# forgebox:begin quick-settings",
  "max_connections = 151",
  "sql_mode = strict",
  "# forgebox:end quick-settings",
  "# your own settings go here",
  "key_buffer_size = 16M",
  "",
].join("\n");

export function configHandlers(data: QaData): FakeHandlers {
  const saved = new Map<string, Map<string, string>>();
  const ports = new Map<string, number>([["mysql/default", 3306]]);
  const policies = new Map<string, T.UpdatePolicy>();
  const current = new Map<string, string>([
    ["mysql", "8.4.3"],
    ["php", "8.3.36"],
  ]);
  const settingsFor = (target: string): T.ServiceSettingsDto => {
    const overrides = saved.get(target);
    const settings = baseSettings().map((setting) => {
      const text = overrides?.get(setting.key);
      if (text === undefined) return setting;
      return {
        ...setting,
        value: typed(setting, text),
        is_default: textOf(setting.default) === text,
      };
    });
    const instance = data.instances.find((i) => `${i.tool}/${i.name}` === target);
    return {
      target,
      tool: target.split(/[/@]/)[0] ?? target,
      settings,
      ports: [
        {
          name: "main",
          port: ports.get(target) ?? 3306,
          default: 3306,
          protocol: "tcp",
          range: "databases",
        },
      ],
      advanced:
        instance === undefined
          ? null
          : {
              autostart: instance.autostart,
              args: [],
              env: {},
              restart_policy: { on: "failure", max: 3, window_secs: 60 },
              restart_policy_default: { on: "failure", max: 3, window_secs: 60 },
              versions: ["8.4.3", "8.4.2"],
            },
    };
  };
  const versions = (id: string): T.ToolVersionDto[] => {
    const list = id === "php" ? ["8.3.36", "8.4.1"] : ["8.4.3", "8.0.40"];
    return list.map((version, index) => ({
      id:
        id === "mysql" && index === 0
          ? fakeId("01QAVERS", 1)
          : fakeId("01QAVERS", 20 + index + (id === "php" ? 10 : 0)),
      tool: id,
      version,
      canonical_name: `${version}-x64`,
      variant: {},
      state: "installed",
      source: "registry",
      path: `C:/Forgebox/bin/${id}/${version}-x64`,
      size_bytes: 200_000_000,
      installed_at: "2026-10-01T10:00:00Z",
      is_current: current.get(id) === version,
    }));
  };
  return {
    "services.settings": ({ target }) => settingsFor(target),
    "services.config_files": ({ target }) => ({
      files: [
        {
          role: "main",
          path: `C:/Forgebox/etc/${target.replace("@", "/")}/my.ini`,
          fs_path: `etc/${target.replace("@", "/")}/my.ini`,
          managed: true,
        },
      ],
    }),
    "services.config": () => ({ files: [] }),
    "services.set": ({ target, settings }) => {
      const map = saved.get(target) ?? new Map<string, string>();
      for (const [key, value] of Object.entries(settings)) {
        if (key.startsWith("ports.")) ports.set(target, Number(value));
        else map.set(key, value);
      }
      saved.set(target, map);
      return fakeJob({ outcome: { done: { instances: data.instances, pools: [] } } });
    },
    "net.port_check": ({ port }) =>
      port === QA_RESERVED_PORT ? { free: false, reserved_by: QA_RESERVED_BY } : { free: true },
    "fs.read": ({ path }) => ({
      path,
      encoding: "utf8",
      content: INI,
      size: INI.length,
      truncated: false,
      modified: null,
      etag: "qa",
    }),
    "tools.info": ({ tool: id }) => {
      const installed = versions(id);
      return {
        tool: {
          id,
          name: id,
          category: null,
          description: null,
          installed,
          current: current.get(id) ?? null,
          policy: policies.get(id) ?? "notify",
          latest: null,
          update_available: null,
          has_service: id !== "php",
        },
        available: [],
        used_by:
          id === "php"
            ? data.sites.slice(0, 1).map((s) => ({ site_id: s.id, host: s.host, version: "8.3" }))
            : [],
      };
    },
    "tools.history": ({ tool: id }) => ({
      tool: id,
      entries:
        id === "mysql"
          ? [
              {
                id: fakeId("01QAUPD", 1),
                series: "8.4",
                from: "8.4.2",
                to: "8.4.3",
                trigger: "user",
                status: "completed",
                started_at: "2026-10-20T09:00:00Z",
                finished_at: "2026-10-20T09:01:00Z",
              },
            ]
          : [],
    }),
    "tools.use": ({ tool: id, version }) => {
      const found = versions(id).find(
        (v) => v.version === version || v.version.startsWith(`${version}.`),
      );
      if (found === undefined) fakeError("not_found", `${id} ${version} is not installed`);
      current.set(id, found.version);
      return { ...found, is_current: true };
    },
    "tools.policy": ({ tool: id, policy }) => {
      policies.set(id, policy);
      return {
        id,
        name: id,
        category: null,
        description: null,
        installed: versions(id),
        current: current.get(id) ?? null,
        policy,
        latest: null,
        update_available: null,
        has_service: id !== "php",
      };
    },
    "tools.check_updates": () => fakeJob({ outcome: { done: { updates: [] } } }),
  };
}
