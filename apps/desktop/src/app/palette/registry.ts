// The command list of the palette (M2-E1-T14): navigation to every section, the global actions
// and the entries that depend on what the daemon reports (start/stop per instance, open a site
// in the browser). `buildCommands` is pure; `CommandPalette` feeds it the stores.
//
// Every command carries the text it is searched by in BOTH shipped locales (pt-BR and en-US),
// so "services" finds "Serviços" and "servi" finds "Services" whatever the UI language is.
import type { MethodName } from "../../ipc/methods";
import type { Action } from "../../actions/registry";
import { restartInstance, startInstance, stopInstance } from "../../actions/services";
import type { InstanceDto, SiteDto } from "../../ipc/generated";
import enPalette from "../../i18n/en-US/palette.json";
import enShell from "../../i18n/en-US/shell.json";
import ptPalette from "../../i18n/pt-BR/palette.json";
import ptShell from "../../i18n/pt-BR/shell.json";
import { FEATURES, type FeatureId } from "../features";
import type { ShortcutId } from "../shortcuts/registry";
import { fold } from "./search";

export type PaletteGroup = "actions" | "navigation" | "services" | "sites";

export interface PaletteCommand {
  id: string;
  group: PaletteGroup;
  /** Localized label shown in the list. */
  label: string;
  /** Folded pt-BR + en-US labels and keywords, what the query is matched against. */
  searchText: string;
  shortcutId?: ShortcutId;
  /** The `forge` command that does the same (null: none). */
  cli: string | null;
  /** Why the command cannot run now (shown instead of running). */
  unavailable: string | null;
  run: () => void;
}

type Translate = (key: string, values?: Record<string, string | number>) => string;

const BUNDLES = {
  "pt-BR": { palette: ptPalette, shell: ptShell },
  "en-US": { palette: enPalette, shell: enShell },
} as const;

/** Dotted lookup in a bundle with `{name}` interpolation (the i18n prefix/suffix of the app). */
function lookup(bundle: unknown, key: string, values: Record<string, string> = {}): string {
  let node: unknown = bundle;
  for (const part of key.split(".")) {
    if (typeof node !== "object" || node === null) return key;
    node = (node as Record<string, unknown>)[part];
  }
  if (typeof node !== "string") return key;
  return node.replace(/\{(\w+)\}/g, (_m, name: string) => values[name] ?? "");
}

/** Both locales' text of a palette key, folded and joined. */
export function bothLocales(
  ns: "palette" | "shell",
  key: string,
  values: Record<string, string> = {},
): string {
  return Object.values(BUNDLES)
    .map((b) => fold(lookup(b[ns], key, values)))
    .join(" ");
}

export interface PaletteInputs {
  t: Translate;
  instances: readonly InstanceDto[];
  sites: readonly SiteDto[];
  /** Why a method is unavailable (`null`: available). */
  unavailable: (method: string) => string | null;
  goTo: (feature: FeatureId) => void;
  /** Opens the detail sheet of an instance on the Services screen. */
  openService: (instanceId: string) => void;
  newSite: () => void;
  run: <M extends MethodName, I>(action: Action<M, I>, input: I, label: string) => Promise<boolean>;
  startAll: (label: string) => Promise<boolean>;
  stopAll: (label: string) => Promise<boolean>;
  openTerminal: (label: string) => Promise<boolean>;
  openUrl: (url: string) => Promise<boolean>;
  /** Renders the CLI of an action (`renderCli`). */
  cli: {
    startAll: string | null;
    stopAll: string | null;
    instance: (action: "start" | "stop" | "restart", target: string) => string | null;
  };
}

/** `tool/name` target of an instance (`mysql/default`). */
export function instanceTarget(instance: InstanceDto): string {
  return `${instance.tool}/${instance.name}`;
}

const RUNNING = new Set(["running", "starting"]);

export function buildCommands(input: PaletteInputs): PaletteCommand[] {
  const { t } = input;
  const out: PaletteCommand[] = [];

  for (const feature of FEATURES) {
    if (feature.id === "onboarding") continue;
    const name = t(`shell:nav.${feature.id}`);
    out.push({
      id: `nav.${feature.id}`,
      group: "navigation",
      label: t("commands.goTo", { name }),
      searchText: [
        bothLocales("palette", "commands.goTo", { name: "" }),
        bothLocales("shell", `nav.${feature.id}`),
        feature.id,
      ].join(" "),
      ...(feature.nav === "none" ? {} : { shortcutId: `nav.${feature.id}` as ShortcutId }),
      cli: null,
      unavailable: null,
      run: () => {
        input.goTo(feature.id);
      },
    });
  }

  const action = (
    id: string,
    key: string,
    method: string | null,
    extra: Partial<PaletteCommand> & { run: () => void },
  ): void => {
    out.push({
      id,
      group: "actions",
      label: t(`commands.${key}`),
      searchText: bothLocales("palette", `commands.${key}`),
      cli: null,
      unavailable: method === null ? null : input.unavailable(method),
      ...extra,
    });
  };
  action("services.startAll", "startAll", "services.start", {
    shortcutId: "services.startAll",
    cli: input.cli.startAll,
    run: () => void input.startAll(t("commands.startAll")),
  });
  action("services.stopAll", "stopAll", "services.stop", {
    shortcutId: "services.stopAll",
    cli: input.cli.stopAll,
    run: () => void input.stopAll(t("commands.stopAll")),
  });
  action("sites.new", "newSite", null, { shortcutId: "sites.new", run: input.newSite });
  action("logs.open", "logs", null, {
    shortcutId: "logs.open",
    run: () => {
      input.goTo("logs");
    },
  });
  action("settings.open", "settings", null, {
    shortcutId: "settings.open",
    run: () => {
      input.goTo("settings");
    },
  });
  action("terminal.open", "openTerminal", "terminal.open", {
    shortcutId: "terminal.open",
    run: () => void input.openTerminal(t("commands.openTerminal")),
  });

  for (const instance of input.instances) {
    const target = instanceTarget(instance);
    const running = RUNNING.has(instance.state);
    const key = running ? "stopInstance" : "startInstance";
    const method = running ? "services.stop" : "services.start";
    const label = t(`commands.${key}`, { name: target });
    out.push({
      id: `instance.${instance.id}.${running ? "stop" : "start"}`,
      group: "services",
      label,
      searchText: [
        bothLocales("palette", `commands.${key}`, { name: target }),
        fold(`${instance.tool} ${instance.name} ${instance.version}`),
      ].join(" "),
      cli: input.cli.instance(running ? "stop" : "start", target),
      unavailable: input.unavailable(method),
      run: () => void input.run(running ? stopInstance : startInstance, target, label),
    });
    // Per-service extras (M2-E2-T12): restart a running one, and open the detail sheet.
    if (running) {
      const restartLabel = t("commands.restartInstance", { name: target });
      out.push({
        id: `instance.${instance.id}.restart`,
        group: "services",
        label: restartLabel,
        searchText: [
          bothLocales("palette", "commands.restartInstance", { name: target }),
          fold(`${instance.tool} ${instance.name} ${instance.version}`),
        ].join(" "),
        cli: input.cli.instance("restart", target),
        unavailable: input.unavailable("services.restart"),
        run: () => void input.run(restartInstance, target, restartLabel),
      });
    }
    out.push({
      id: `instance.${instance.id}.open`,
      group: "services",
      label: t("commands.openInstance", { name: target }),
      searchText: [
        bothLocales("palette", "commands.openInstance", { name: target }),
        fold(`${instance.tool} ${instance.name} ${instance.version}`),
      ].join(" "),
      cli: null,
      unavailable: null,
      run: () => {
        input.openService(instance.id);
      },
    });
  }

  for (const site of input.sites) {
    const label = t("commands.openSite", { host: site.host });
    out.push({
      id: `site.${site.id}.open`,
      group: "sites",
      label,
      searchText: [
        bothLocales("palette", "commands.openSite", { host: site.host }),
        fold(site.host),
      ].join(" "),
      cli: null,
      unavailable: null,
      run: () => void input.openUrl(site.url),
    });
  }
  return out;
}
