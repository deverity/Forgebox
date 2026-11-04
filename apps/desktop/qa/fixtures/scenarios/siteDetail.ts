// Handlers of the Site detail (M2-E2-T16): the live PHP switch (`sites.php` with its
// `site.php_changed` before `job.done`, `version_not_installed`, `tools.plan`/`tools.install`),
// `sites.ps`/`sites.run` with `service.*` events carrying `site_id`, `sites.info` checks,
// `sites.open`, `sites.driver`, `tls.list` and `net.dns_test`, over the QA data.
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import type { QaData } from "./data";

/** Series that fail to switch: the pool never comes up (`job.failed`, the row is untouched). */
export const QA_BROKEN_SERIES = "7.4";
/** Series offered by the providers and not installed. */
export const QA_AVAILABLE_SERIES = ["8.5", "8.4", "8.3", "8.2", "7.4"];
/** `tools.info` of PHP lists these as available (see config.ts). */
export const QA_AVAILABLE_PHP = QA_AVAILABLE_SERIES.map((series) => ({
  version: `${series}.0`,
  variants: ["nts-vs17-x64"],
  channel: "stable",
}));
/** Size `tools.plan` reports for a PHP build. */
export const QA_PHP_BYTES = 33_554_432;

const STEP_MS = 60;

function phpVersion(version: string): T.ToolVersionDto {
  return {
    id: `01QAVERS${version.replaceAll(".", "").padEnd(18, "0")}`,
    tool: "php",
    version,
    canonical_name: `${version}-nts-vs17-x64`,
    variant: { ts: "nts", vc: "vs17", arch: "x64" },
    state: "installed",
    source: "registry",
    path: `C:/Forgebox/bin/php/${version}-nts-vs17-x64`,
    size_bytes: QA_PHP_BYTES,
    installed_at: "2026-10-28T10:00:00Z",
    is_current: false,
  };
}

/** Seeds the PHP tool with two installed series when the scenario has the tool but no versions. */
export function seedPhp(data: QaData): void {
  const php = data.tools.find((tool) => tool.id === "php");
  if (php?.installed.length === 0) {
    php.installed = [phpVersion("8.3.36"), phpVersion("8.2.30")];
  }
}

function installedSeries(data: QaData): string[] {
  const php = data.tools.find((tool) => tool.id === "php");
  return (php?.installed ?? []).map((v) => v.version.split(".").slice(0, 2).join("."));
}

function findSite(data: QaData, ref: string): T.SiteDto {
  const found = data.sites.find((s) => s.id === ref || s.host === ref);
  if (found === undefined) fakeError("not_found", `site ${ref} not found`);
  return found;
}

/** Secret values of the QA `.env` (only sent with `reveal: true`). */
export const QA_ENV_SECRET = "s3cr3t-qa-value";

function qaEnvFiles(reveal: boolean): T.DotenvFileDto[] {
  const secret = (key: string, line: number, value: string): T.DotenvVarDto => ({
    key,
    value: reveal ? value : null,
    secret: true,
    line,
  });
  return [
    {
      name: ".env",
      path: "C:/Forgebox/www/loja.test/.env",
      vars: [
        { key: "APP_ENV", value: "local", secret: false, line: 1 },
        { key: "APP_URL", value: "https://loja.test", secret: false, line: 2 },
        secret("DB_PASSWORD", 3, QA_ENV_SECRET),
        secret("APP_KEY", 4, "base64:qa-app-key"),
      ],
    },
  ];
}

export function siteDetailHandlers(data: QaData): FakeHandlers {
  seedPhp(data);
  return {
    "sites.php": (params, ctx) => {
      const site = findSite(data, params.site);
      const series = params.php.split(".").slice(0, 2).join(".");
      if (!installedSeries(data).includes(series)) {
        fakeError("not_found", `PHP ${params.php} is not installed`, "version_not_installed", {
          tool: "php",
          requested: params.php,
          installed: data.tools.find((t) => t.id === "php")?.installed.map((v) => v.version) ?? [],
          fix: `forge tool install php@${series}`,
        });
      }
      if (series === QA_BROKEN_SERIES) {
        return fakeJob({
          steps: [{ pct: 30, message: "starting pool" }],
          stepMs: STEP_MS,
          outcome: {
            failed: {
              code: "internal",
              message: `pool php_${series.replace(".", "_")} did not start`,
            },
          },
        });
      }
      const from = site.php?.resolved ?? null;
      const to = `${series}.0`;
      const next: T.SiteDto = {
        ...site,
        php: { series, exact: null, resolved: to, upstream: `php_${series.replace(".", "_")}` },
      };
      // `site.php_changed` goes out after the swap and before `job.done`, like the daemon.
      setTimeout(() => {
        data.sites.splice(data.sites.indexOf(site), 1, next);
        ctx.fake.publish("site.php_changed", {
          v: 1,
          site_id: site.id,
          host: site.host,
          from,
          to,
        });
      }, STEP_MS * 2.5);
      return fakeJob({
        steps: [
          { pct: 25, message: "starting pool" },
          { pct: 75, message: "reloading" },
        ],
        stepMs: STEP_MS,
        outcome: { done: next },
      });
    },
    "tools.plan": (params) => {
      const spec = params.specs?.[0] ?? "php";
      return fakeJob({
        stepMs: STEP_MS / 2,
        outcome: {
          done: {
            items: [
              {
                tool: "php",
                spec,
                version: `${spec.split("@")[1] ?? "8.4"}.0`,
                size_bytes: QA_PHP_BYTES,
                size_estimated: false,
                reason: "requested",
              },
            ],
            total_bytes: QA_PHP_BYTES,
          },
        },
      });
    },
    "tools.install": (params) => {
      const versions = params.specs.map((spec) => `${spec.split("@")[1] ?? "8.4"}.0`);
      const php = data.tools.find((tool) => tool.id === "php");
      for (const version of versions) {
        if (php !== undefined && !php.installed.some((v) => v.version === version)) {
          php.installed.push(phpVersion(version));
        }
      }
      return fakeJob({
        steps: [
          { pct: 40, message: "downloading" },
          { pct: 90, message: "extracting" },
        ],
        stepMs: STEP_MS,
        outcome: { done: { installed: versions.map(phpVersion) } },
      });
    },
    "sites.ps": (params) => {
      const site = findSite(data, params.site ?? "");
      return { processes: site.processes };
    },
    "sites.run": (params, ctx) => {
      const site = findSite(data, params.site);
      const process = site.processes.find((p) => p.name === params.process);
      if (process === undefined) fakeError("not_found", `process ${params.process} not found`);
      const instanceId =
        process.instance_id ?? `01QAPROC${process.name.toUpperCase().padEnd(18, "0")}`;
      const payload = (state: T.ServiceState, pid: number | null) => ({
        v: 1,
        instance_id: instanceId,
        name: `${site.host}/${process.name}`,
        tool: "procfile",
        state,
        pid,
        strategy: null,
        exit_code: null,
        error: null,
        site_id: site.id,
      });
      const set = (state: T.ServiceState, pid: number | null) => {
        process.state = state;
        process.pid = pid;
        process.instance_id = instanceId;
      };
      if (params.action === "stop") {
        setTimeout(() => {
          set("stopped", null);
          ctx.fake.publish("service.stopped", payload("stopped", null));
        }, STEP_MS);
      } else {
        setTimeout(() => {
          set("starting", null);
          ctx.fake.publish("service.starting", payload("starting", null));
        }, STEP_MS / 2);
        setTimeout(() => {
          set("running", 7000);
          ctx.fake.publish("service.running", payload("running", 7000));
        }, STEP_MS * 1.5);
      }
      return fakeJob({ stepMs: STEP_MS * 2, outcome: { done: { ...process } } });
    },
    "sites.info": (params) => {
      const site = findSite(data, params.site);
      const checks: T.CheckDto[] = [
        { id: "site.dns", status: "ok", message: `${site.host} resolves to 127.0.0.1`, fix: null },
        {
          id: "site.root_access",
          status: "warn",
          message: "The folder is not case-sensitive",
          fix: `fsutil.exe file setCaseSensitiveInfo ${site.root ?? ""} enable`,
        },
        {
          id: "site.php_pool",
          status: site.php === null ? "skipped" : "fail",
          message: "The pool is not running",
          fix: "forge start php",
        },
        {
          id: "site.certificate",
          status: "ok",
          message: "Certificate valid for 300 days",
          fix: null,
        },
      ];
      return { site, checks };
    },
    "sites.env": (params) => {
      const site = findSite(data, params.site);
      // Only the first QA site has a `.env`; the others show the empty state.
      const has = data.sites[0]?.id === site.id;
      return {
        site_id: site.id,
        host: site.host,
        files: has ? qaEnvFiles(params.reveal === true) : [],
        revealed: has && params.reveal === true,
      };
    },
    "shell.open_editor": () => ({ ok: true, message: null }),
    "sites.open": () => ({ ok: true, message: null }),
    "sites.driver": (params, ctx) => {
      const site = findSite(data, params.site);
      const next = { ...site, driver: params.driver ?? "generic-php" };
      data.sites.splice(data.sites.indexOf(site), 1, next);
      ctx.fake.publish("site.updated", { v: 1, site_id: site.id, host: site.host });
      return fakeJob({ outcome: { done: next } });
    },
    "tls.list": () => ({
      certificates: data.sites
        .filter((site) => site.secure)
        .map((site) => ({
          host: site.host,
          sans: [site.host],
          site_id: site.id,
          not_before: "2026-10-01T00:00:00Z",
          not_after: "2027-08-28T00:00:00Z",
          thumbprint_sha256: "CD",
          ca_thumbprint_sha256: "AB",
          trusted: true,
        })),
    }),
    "net.dns_test": (params) => ({
      host: params.host,
      resolved: ["127.0.0.1"],
      expected: ["127.0.0.1"],
      ok: true,
      via: "forgebox-dns",
    }),
  };
}
