// Handlers of Settings > Sistema de arquivos, Registries and Atualizacoes (M2-E3-T18):
// `fs_parity.case_*` (status per folder, convert/revert job with progress, conflict and cancel),
// `fs_parity.lint_case` (dry run and apply), `registry.*` (list, add, remove, update job),
// `tools.list|policy|check_updates`. State lives in the closure; tests read `state.calls`.
//
// Registered as the `filesystem` scenario (its own: it owns `tools.list`/`tools.policy`).
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import { tool } from "./data";

const STEP_MS = 120;
const HOUR_MS = 3_600_000;

export const QA_LINT_ROOT = "C:/Forgebox/www/loja";

/** How the convert job ends. `hold`: runs until the test cancels it or completes it. */
export type ConvertMode = "ok" | "conflict" | "in_use" | "hold";

export interface FilesystemState {
  /** Case state per folder name (`www`...). `null` = unsupported. */
  dirs: Record<string, boolean | null>;
  convert: ConvertMode;
  registries: T.RegistryDto[];
  policies: Record<string, T.UpdatePolicy>;
  /** Tools reported with an update by `tools.check_updates`. */
  updates: T.ToolUpdateDto[];
  /** Methods called, with their params. */
  calls: { method: string; params: unknown }[];
}

export interface FilesystemQa {
  handlers: FakeHandlers;
  state: FilesystemState;
}

function registry(patch: Partial<T.RegistryDto> & { name: string }): T.RegistryDto {
  return {
    url: `https://example.org/${patch.name}/index.yaml`,
    kind: "http",
    official: false,
    enabled: true,
    priority: 10,
    tools: 5,
    last_refresh_at: new Date(Date.now() - 3 * HOUR_MS).toISOString(),
    trusted_unsigned: false,
    verified: true,
    key_id: "FB01",
    last_error: null,
    ...patch,
  };
}

function version(toolId: string, value: string): T.ToolVersionDto {
  return {
    id: `01QAVER${toolId}${value}`.slice(0, 26).padEnd(26, "0"),
    tool: toolId,
    version: value,
    canonical_name: value,
    variant: {},
    state: "installed",
    source: "registry",
    path: `C:/Forgebox/bin/${toolId}/${value}`,
    size_bytes: null,
    installed_at: null,
    is_current: true,
  };
}

const NAME_OF = (dir: string) =>
  dir
    .replace(/[\\/]+$/, "")
    .split(/[\\/]/)
    .pop() ?? dir;

function lintReport(dryRun: boolean): T.CaseLintReportDto {
  const base = {
    category: "mismatch",
    column: 9,
    ambiguous: false,
    detail: null,
    target: null,
  } as const;
  const issues: T.CaseIssueDto[] = [
    {
      ...base,
      id: "app/index.php:12:9",
      file: "app/index.php",
      line: 12,
      reference: "Views/header.php",
      actual: "views/Header.php",
      kind: "php-include",
      expected: "Views/header.php",
      fixes: ["rename", "edit"],
      fix: {
        strategy: "rename",
        from: "views/Header.php",
        to: "Views/header.php",
        before: null,
        after: null,
        applied: !dryRun,
        error: null,
      },
      fixed: !dryRun,
    },
    {
      ...base,
      id: "resources/app.js:3:9",
      file: "resources/app.js",
      line: 3,
      reference: "./Utils.js",
      actual: "./utils.js",
      kind: "js-import",
      expected: null,
      fixes: ["edit"],
      fix: {
        strategy: "edit",
        from: "./Utils.js",
        to: "./utils.js",
        before: 'import { go } from "./Utils.js";',
        after: 'import { go } from "./utils.js";',
        applied: !dryRun,
        error: null,
      },
      fixed: !dryRun,
    },
    {
      ...base,
      id: "storage/Logo.png",
      category: "case_duplicate",
      file: "storage/Logo.png",
      line: 0,
      column: 0,
      reference: "Logo.png",
      actual: "logo.png",
      kind: "fs",
      expected: null,
      fixes: [],
      fix: null,
      fixed: false,
    },
  ];
  return {
    v: 1,
    root: QA_LINT_ROOT,
    issues,
    files_scanned: 214,
    references_checked: 980,
    fix: true,
    dry_run: dryRun,
    remaining: dryRun ? null : 1,
  };
}

export function createFilesystemQa(initial: { convert?: ConvertMode } = {}): FilesystemQa {
  const state: FilesystemState = {
    dirs: { www: true, etc: true, data: false, logs: true, tmp: null },
    convert: initial.convert ?? "ok",
    registries: [
      registry({
        name: "official",
        url: "https://github.com/forgebox/registry.git",
        kind: "git",
        official: true,
        tools: 42,
      }),
      registry({ name: "acme", tools: 6 }),
      registry({
        name: "lab",
        url: "http://192.168.0.5/index.yaml",
        tools: 0,
        last_refresh_at: null,
        trusted_unsigned: true,
        verified: false,
        key_id: null,
      }),
      // A signed internal mirror (docs/05): a dir registry that is NOT unverified.
      registry({ name: "mirror", url: "file:///C:/mirror", kind: "dir", tools: 9 }),
      // Refreshed once, failing since.
      registry({ name: "flaky", tools: 3, last_error: "index.yaml: HTTP 503" }),
    ],
    policies: { php: "notify", nginx: "auto-patch", mysql: "manual" },
    updates: [{ tool: "php", from: "8.4.26", to: "8.4.27", automatic: false }],
    calls: [],
  };
  const record = (method: string, params: unknown) => {
    state.calls.push({ method, params });
  };
  const status = (dir: string): T.CaseSensitivityDto => {
    const enabled = state.dirs[NAME_OF(dir)] ?? null;
    return enabled === null
      ? {
          dir,
          supported: false,
          enabled: null,
          fs_type: "ReFS",
          os_build: 19045,
          win_version_ok: true,
          reason: "Per-folder case sensitivity needs NTFS",
        }
      : {
          dir,
          supported: true,
          enabled,
          fs_type: "NTFS",
          os_build: 19045,
          win_version_ok: true,
          reason: null,
        };
  };
  const tools = (): T.ToolDto[] =>
    ["php", "nginx", "mysql"].map((id) =>
      tool(id, {
        name: id === "php" ? "PHP" : id === "nginx" ? "Nginx" : "MySQL",
        installed: [version(id, "1.0.0")],
        policy: state.policies[id] ?? "notify",
        update_available: id === "php" ? "8.4.27" : null,
      }),
    );

  const handlers: FakeHandlers = {
    "fs_parity.case_status": ({ dir }) => status(dir),
    "fs_parity.case_enable": ({ dir }) => {
      record("fs_parity.case_enable", { dir });
      state.dirs[NAME_OF(dir)] = true;
      return status(dir);
    },
    "fs_parity.case_disable": ({ dir }) => {
      record("fs_parity.case_disable", { dir });
      state.dirs[NAME_OF(dir)] = false;
      return status(dir);
    },
    "fs_parity.case_convert": (params) => {
      record("fs_parity.case_convert", params);
      const revert = params.revert === true;
      const name = NAME_OF(params.dir);
      const report: T.CaseConvertDto = {
        dir: params.dir,
        enabled: !revert,
        directories: 48,
        files: 1203,
        links: 0,
        bytes: 52_428_800,
        sampled: 10,
        already: false,
        recovered: false,
        leftover: null,
        duration_ms: 900,
      };
      const steps = [
        { pct: 20, message: "Scanning the folder" },
        { pct: 55, message: "Moving files" },
        { pct: 90, message: "Checking the checksums" },
      ];
      if (revert && state.convert === "conflict") {
        return fakeJob({
          stepMs: STEP_MS,
          steps,
          outcome: {
            failed: {
              code: "conflict",
              message: "names differ only by case: Readme.md and readme.md",
              details: {
                reason: "case_collisions",
                args: { examples: "Readme.md; readme.md", count: 1 },
              },
            },
          },
        });
      }
      if (revert && state.convert === "in_use") {
        return fakeJob({
          stepMs: STEP_MS,
          steps,
          outcome: {
            failed: {
              code: "conflict",
              message: "C:/Forgebox/www/a.php is in use by another process",
            },
          },
        });
      }
      if (state.convert === "hold") {
        return fakeJob({
          stepMs: STEP_MS,
          steps: [steps[0] as T.JobProgress],
          hold: true,
          outcome: { done: report },
          onFinish: (outcome) => {
            if ("done" in outcome) state.dirs[name] = !revert;
          },
        });
      }
      return fakeJob({
        stepMs: STEP_MS,
        steps,
        outcome: { done: report },
        onFinish: (outcome) => {
          if ("done" in outcome) state.dirs[name] = !revert;
        },
      });
    },
    "fs_parity.lint_case": (params) => {
      record("fs_parity.lint_case", params);
      const dryRun = params.dry_run === true;
      return fakeJob({
        stepMs: STEP_MS,
        steps: [{ pct: null, message: "Reading the project files" }],
        outcome: { done: lintReport(dryRun) },
      });
    },
    "shell.open_editor": (params) => {
      record("shell.open_editor", params);
      return { ok: true, message: null };
    },
    "registry.list": () => ({ registries: state.registries.map((r) => ({ ...r })) }),
    "registry.add": (params) => {
      record("registry.add", params);
      const url = params.url.trim();
      if (!/^(https?:\/\/|git\+|file:|[A-Za-z]:[\\/])/.test(url)) {
        fakeError("invalid_input", `invalid URL \`${url}\``);
      }
      const name = params.name ?? new URL(url.replace(/^git\+/, "")).hostname.split(".")[0] ?? "x";
      if (state.registries.some((r) => r.name === name)) {
        fakeError("conflict", `registry ${name} already exists`);
      }
      const added = registry({ name, url, tools: 2 });
      state.registries.push(added);
      return added;
    },
    "registry.remove": (params) => {
      record("registry.remove", params);
      if (params.name === "official") {
        fakeError("conflict", "the official registry cannot be removed");
      }
      state.registries = state.registries.filter((r) => r.name !== params.name);
      return { ok: true, message: null };
    },
    "registry.update": () => {
      record("registry.update", {});
      return fakeJob({
        stepMs: STEP_MS,
        steps: [
          { pct: 30, message: "Refreshing official" },
          { pct: 70, message: "Refreshing acme" },
        ],
        outcome: { done: { registries: state.registries } },
        onFinish: (outcome) => {
          if (!("done" in outcome)) return;
          const now = new Date().toISOString();
          state.registries = state.registries.map((r) => ({
            ...r,
            last_refresh_at: r.name === "lab" ? r.last_refresh_at : now,
          }));
        },
      });
    },
    "tools.list": () => ({ tools: tools() }),
    "tools.policy": (params) => {
      record("tools.policy", params);
      state.policies[params.tool] = params.policy;
      const found = tools().find((t) => t.id === params.tool);
      if (found === undefined) fakeError("not_found", `unknown tool ${params.tool}`);
      return found;
    },
    "tools.check_updates": (params) => {
      record("tools.check_updates", params);
      return fakeJob({
        stepMs: STEP_MS,
        steps: [{ pct: 50, message: "Asking the registries" }],
        outcome: { done: { updates: state.updates } },
      });
    },
  };
  return { handlers, state };
}
