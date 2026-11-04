// Handlers of the PHP screen (M2-E2-T17): `php.list` with three installed series and their
// pools, 60 extensions per series, optimistic toggles that can fail, Xdebug with an install
// that reports progress, `tools.use` with `tool.current_changed`, update history and rollback,
// and the quick settings of `php@<series>`. Services, sites and the tools of other screens are
// delegated to `config` so the rest of the QA data is untouched.
import { fakeError, fakeJob, type FakeHandlers, type FakeJob } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import { fakeId, type QaData } from "./data";

/** Toggling this extension always fails (`job.failed`), to see the rollback. */
export const QA_FAILING_EXTENSION = "sodium";
/** Series without Xdebug: choosing a mode installs it first. */
export const QA_NO_XDEBUG_SERIES = "8.4";

const STEP_MS = 120;
const PHP = "php";

const BUNDLED = [
  "bcmath",
  "bz2",
  "calendar",
  "ctype",
  "curl",
  "dba",
  "dom",
  "enchant",
  "exif",
  "ffi",
  "fileinfo",
  "filter",
  "ftp",
  "gd",
  "gettext",
  "gmp",
  "iconv",
  "imap",
  "intl",
  "json",
  "ldap",
  "mbstring",
  "mysqli",
  "mysqlnd",
  "odbc",
  "openssl",
  "pcntl",
  "pdo_firebird",
  "pdo_mysql",
  "pdo_oci",
  "pdo_odbc",
  "pdo_pgsql",
  "pdo_sqlite",
  "pgsql",
  "phar",
  "posix",
  "readline",
  "session",
  "shmop",
  "simplexml",
  "snmp",
  "soap",
  "sockets",
  "sodium",
  "spl",
  "sqlite3",
  "standard",
  "sysvmsg",
  "tidy",
  "tokenizer",
  "xml",
  "xmlreader",
  "xmlwriter",
  "xsl",
  "zip",
  "zlib",
];
/** Enabled out of the box. */
const ON_BY_DEFAULT = new Set([
  "ctype",
  "curl",
  "dom",
  "fileinfo",
  "filter",
  "iconv",
  "intl",
  "json",
  "mbstring",
  "mysqli",
  "mysqlnd",
  "openssl",
  "pdo_mysql",
  "pdo_sqlite",
  "phar",
  "session",
  "simplexml",
  "tokenizer",
  "xml",
  "xmlreader",
  "xmlwriter",
  "zip",
  "zlib",
]);

interface SeriesState {
  series: string;
  version: string;
  extensions: T.PhpExtDto[];
  xdebug: T.XdebugMode;
  pool: T.PoolDto | null;
  sites: number;
  settings: Map<string, string>;
}

function extensionList(series: string): T.PhpExtDto[] {
  const list: T.PhpExtDto[] = BUNDLED.map((name) => ({
    name,
    enabled: ON_BY_DEFAULT.has(name),
    bundled: true,
    zend: false,
  }));
  list.push({ name: "opcache", enabled: true, bundled: true, zend: true });
  list.push({ name: "redis", enabled: series === "8.3", bundled: false, zend: false });
  list.push({ name: "imagick", enabled: false, bundled: false, zend: false });
  if (series !== QA_NO_XDEBUG_SERIES) {
    list.push({ name: "xdebug", enabled: false, bundled: false, zend: true });
  }
  return list;
}

function poolOf(
  n: number,
  series: string,
  version: string,
  state: T.ServiceState,
  busy: number,
): T.PoolDto {
  return {
    series,
    version,
    version_id: fakeId("01QAPHP", n),
    state,
    upstream: `php_${series.replace(".", "_")}`,
    workers: 4,
    busy_workers: busy,
    min_workers: 2,
    max_workers: 8,
    ports: [9100 + n * 2, 9101 + n * 2],
    sites: 1,
    xdebug: false,
    requests_total: 1000 * n,
    rss_bytes: 48_000_000 + n * 4_000_000,
  };
}

function seriesState(n: number, series: string, version: string): SeriesState {
  return {
    series,
    version,
    extensions: extensionList(series),
    xdebug: "off",
    pool: n === 1 ? null : poolOf(n, series, version, "running", n === 2 ? 1 : 0),
    sites: n === 2 ? 1 : 0,
    settings: new Map(),
  };
}

function phpSettings(state: SeriesState): T.QuickSettingDto[] {
  const base = [
    {
      key: "memory_limit",
      label: "Memory limit",
      kind: "text",
      value: "512M",
      default: "512M",
      apply: "reload",
    },
    {
      key: "upload_max_filesize",
      label: "Upload max filesize",
      kind: "text",
      value: "64M",
      default: "64M",
      apply: "reload",
    },
    {
      key: "max_execution_time",
      label: "Max execution time",
      kind: "number",
      min: 0,
      max: 3600,
      value: 60,
      default: 60,
      apply: "reload",
    },
    {
      key: "display_errors",
      label: "Display errors",
      kind: "toggle",
      value: true,
      default: true,
      apply: "reload",
    },
    {
      key: "opcache_enable",
      label: "OPcache",
      kind: "toggle",
      value: true,
      default: true,
      apply: "reload",
    },
  ] as const;
  return base.map((setting): T.QuickSettingDto => {
    const text = state.settings.get(setting.key);
    if (text === undefined) return { ...setting, is_default: true };
    const value: T.SettingValueDto =
      setting.kind === "toggle" ? text === "true" : setting.kind === "number" ? Number(text) : text;
    return { ...setting, value, is_default: String(setting.default) === text };
  });
}

export function phpHandlers(data: QaData, config: FakeHandlers): FakeHandlers {
  const states = [
    seriesState(1, "8.2", "8.2.30"),
    seriesState(2, "8.3", "8.3.36"),
    seriesState(3, "8.4", "8.4.1"),
  ];
  let cli = "8.3.36";
  const find = (version: string): SeriesState => {
    const series = version.split(".").slice(0, 2).join(".");
    const found = states.find((s) => s.series === series);
    if (found === undefined) fakeError("not_found", `PHP ${version} is not installed`);
    return found;
  };
  const versionDto = (s: SeriesState): T.PhpVersionDto => ({
    version_id: fakeId("01QAPHP", states.indexOf(s) + 1),
    version: s.version,
    series: s.series,
    thread_safety: "nts",
    compiler: "vs17",
    arch: "x64",
    is_current_cli: cli === s.version,
    pool: s.pool === null ? null : { ...s.pool, xdebug: s.xdebug !== "off" },
    sites: s.sites,
    xdebug: s.xdebug,
  });
  const history: T.UpdateHistoryEntryDto[] = [
    {
      id: fakeId("01QAPUPD", 1),
      series: "8.3",
      from: "8.3.35",
      to: "8.3.36",
      trigger: "user",
      status: "completed",
      started_at: "2026-10-20T09:00:00Z",
      finished_at: "2026-10-20T09:01:00Z",
    },
  ];
  const toolsUse = config["tools.use"];
  const toolsHistory = config["tools.history"];
  function toggle(version: string, extension: string, enabled: boolean): FakeJob<T.PhpExtListDto> {
    const s = find(version);
    const found = s.extensions.find((e) => e.name === extension);
    if (found === undefined) fakeError("not_found", `extension ${extension} not found`);
    if (extension === QA_FAILING_EXTENSION) {
      return fakeJob<T.PhpExtListDto>({
        stepMs: STEP_MS,
        outcome: {
          failed: {
            code: "internal",
            message: `the pool php_${s.series.replace(".", "_")} did not reload with ${extension}`,
          },
        },
      });
    }
    found.enabled = enabled;
    return fakeJob({
      stepMs: STEP_MS,
      outcome: { done: { version: s.series, extensions: s.extensions } },
    });
  }
  return {
    // A scenario without the PHP tool (empty, heavy) has no PHP installed.
    "php.list": () => ({
      versions: data.tools.some((tool) => tool.id === PHP) ? states.map(versionDto) : [],
    }),
    "php.ext_list": ({ version }) => ({ version, extensions: find(version).extensions }),
    "php.ext_enable": ({ version, extension }) => toggle(version, extension, true),
    "php.ext_disable": ({ version, extension }) => toggle(version, extension, false),
    "php.xdebug": ({ version, mode, port }, ctx) => {
      const s = find(version);
      if (port !== undefined && port !== null && port < 1024) {
        fakeError("conflict", `port ${String(port)} is reserved`, "port_reserved", { port });
      }
      const missing = !s.extensions.some((e) => e.name === "xdebug");
      const steps: { pct: number; message: string; bytes_done?: number; bytes_total?: number }[] =
        missing
          ? [
              { pct: 20, message: "downloading", bytes_done: 400_000, bytes_total: 2_000_000 },
              { pct: 70, message: "downloading", bytes_done: 1_400_000, bytes_total: 2_000_000 },
              { pct: 95, message: "installing" },
            ]
          : [{ pct: 50, message: "rendering" }];
      setTimeout(
        () => {
          if (missing) {
            s.extensions.push({ name: "xdebug", enabled: false, bundled: false, zend: true });
          }
          s.xdebug = mode;
          const xdebug = s.extensions.find((e) => e.name === "xdebug");
          if (xdebug !== undefined) xdebug.enabled = mode !== "off";
          ctx.fake.publish("pool.scaled", {
            v: 1,
            version_id: fakeId("01QAPHP", states.indexOf(s) + 1),
            series: s.series,
            unknown: [],
          });
        },
        STEP_MS * (steps.length + 0.5),
      );
      return fakeJob({
        steps,
        stepMs: STEP_MS,
        outcome: {
          done: poolOf(states.indexOf(s) + 1, s.series, s.version, "running", 0),
        },
      });
    },
    "php.pool_status": () => ({ pools: states.flatMap((s) => (s.pool === null ? [] : [s.pool])) }),
    "services.settings": (params, ctx) => {
      if (!params.target.startsWith(`${PHP}@`)) {
        const fallback = config["services.settings"];
        if (fallback === undefined) fakeError("not_found", `${params.target} not found`);
        return fallback(params, ctx);
      }
      const s = find(params.target.slice(PHP.length + 1));
      return {
        target: params.target,
        tool: PHP,
        settings: phpSettings(s),
        ports: [],
        advanced: null,
      };
    },
    "services.set": (params, ctx) => {
      if (!params.target.startsWith(`${PHP}@`)) {
        const fallback = config["services.set"];
        if (fallback === undefined) fakeError("not_found", `${params.target} not found`);
        return fallback(params, ctx);
      }
      const s = find(params.target.slice(PHP.length + 1));
      for (const [key, value] of Object.entries(params.settings)) s.settings.set(key, value);
      return fakeJob({ outcome: { done: { instances: data.instances, pools: [] } } });
    },
    "tools.use": (params, ctx) => {
      if (params.tool !== PHP) {
        if (toolsUse === undefined) fakeError("not_found", `${params.tool} not found`);
        return toolsUse(params, ctx);
      }
      const s = find(params.version);
      const from = cli;
      cli = s.version;
      const phpTool = data.tools.find((tool) => tool.id === PHP);
      if (phpTool !== undefined) phpTool.current = s.version;
      ctx.fake.publish("tool.current_changed", {
        v: 1,
        tool: PHP,
        from: `${from}-nts-vs17-x64`,
        to: `${s.version}-nts-vs17-x64`,
        version_id: fakeId("01QAPHP", states.indexOf(s) + 1),
        duration_ms: 3,
      });
      return {
        id: fakeId("01QAPHP", states.indexOf(s) + 1),
        tool: PHP,
        version: s.version,
        canonical_name: `${s.version}-nts-vs17-x64`,
        variant: { ts: "nts", vc: "vs17", arch: "x64" },
        state: "installed",
        source: "registry",
        path: `C:/Forgebox/bin/php/${s.version}-nts-vs17-x64`,
        size_bytes: 33_554_432,
        installed_at: "2026-10-28T10:00:00Z",
        is_current: true,
      };
    },
    "tools.history": (params, ctx) => {
      if (params.tool !== PHP) {
        if (toolsHistory === undefined) return { tool: params.tool, entries: [] };
        return toolsHistory(params, ctx);
      }
      const series = params.series ?? null;
      return {
        tool: PHP,
        entries: history.filter((entry) => series === null || entry.series === series),
      };
    },
    "tools.check_updates": (params, ctx) => {
      if (params.tool !== PHP) {
        const fallback = config["tools.check_updates"];
        if (fallback === undefined) return fakeJob({ outcome: { done: { updates: [] } } });
        return fallback(params, ctx);
      }
      return fakeJob({
        stepMs: STEP_MS,
        outcome: {
          done: { updates: [{ tool: PHP, from: "8.3.36", to: "8.3.37", automatic: false }] },
        },
      });
    },
    "tools.rollback": ({ spec }) => {
      const s = find(spec.split("@")[1] ?? "");
      history.unshift({
        id: fakeId("01QAPUPD", history.length + 1),
        series: s.series,
        from: s.version,
        to: "8.3.35",
        trigger: "user",
        status: "completed",
        rollback_of: history[0]?.id ?? null,
        started_at: "2026-10-28T11:00:00Z",
        finished_at: "2026-10-28T11:01:00Z",
      });
      return fakeJob({
        stepMs: STEP_MS,
        outcome: {
          done: {
            id: fakeId("01QAPHP", states.indexOf(s) + 1),
            tool: PHP,
            version: "8.3.35",
            canonical_name: "8.3.35-nts-vs17-x64",
            variant: {},
            state: "installed",
            source: "registry",
            path: "C:/Forgebox/bin/php/8.3.35-nts-vs17-x64",
            size_bytes: null,
            installed_at: null,
            is_current: false,
          },
        },
      });
    },
  };
}
