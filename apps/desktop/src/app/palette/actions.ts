// What a shortcut or a palette entry does (M2-E1-T14). Mutating work goes through `runAction`
// (the reconnect queue, the same path as ActionButton) and reports with toasts: started, done or
// the localized cause. Kept free of the palette's search bundles so the shell can load it eagerly.
import { useNavigate } from "@tanstack/react-router";
import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { runAction, type Action } from "../../actions/registry";
import { startAllServices, stopAllServices } from "../../actions/services";
import { openTerminal } from "../../actions/terminal";
import type { MethodName } from "../../ipc/methods";
import { toast } from "../../design/primitives/toastStore";
import { useErrorMessage, useErrorTranslate } from "../shell/useErrorText";
import { useCapabilities, useRuntime } from "../../state";
import { featureOf, type FeatureId } from "../features";
import { DEFAULT_SHORTCUTS } from "../shortcuts/registry";
import type { ShortcutHandlers } from "../shortcuts/useShortcuts";

/** DOM event for a "new site" request; the Sites screen (M2-E2-T15) listens to it. */
export const NEW_SITE_EVENT = "forgebox:new-site";

let newSitePending = false;

/**
 * True once after a "new site" request that the Sites screen has not handled yet (its lazy
 * route chunk may mount after the event fired). The screen calls it on mount.
 */
export function takePendingNewSite(): boolean {
  const pending = newSitePending;
  newSitePending = false;
  return pending;
}

type Translate = (key: string, values?: Record<string, string | number>) => string;

export interface PaletteActions {
  goTo: (feature: FeatureId) => void;
  newSite: () => void;
  /** Runs a registered action with job toasts; resolves when it ended (never rejects). */
  run: <M extends MethodName, I>(action: Action<M, I>, input: I, label: string) => Promise<boolean>;
  startAll: (label: string) => Promise<boolean>;
  stopAll: (label: string) => Promise<boolean>;
  openTerminal: (label: string) => Promise<boolean>;
  /** The in-app handler of every shortcut id (labels are the localized command names). */
  handlers: () => ShortcutHandlers;
}

export function usePaletteActions(onTogglePalette: () => void): PaletteActions {
  const { t } = useTranslation("palette");
  const tx = t as unknown as Translate;
  const runtime = useRuntime();
  const navigate = useNavigate();
  const errorMessage = useErrorMessage();
  const capabilities = useCapabilities();
  const translateError = useErrorTranslate();

  const goTo = useCallback(
    (feature: FeatureId) => {
      void navigate({ to: featureOf(feature).path });
    },
    [navigate],
  );

  const run = useCallback(
    async <M extends MethodName, I>(
      action: Action<M, I>,
      input: I,
      label: string,
    ): Promise<boolean> => {
      toast.show({ kind: "info", message: tx("toast.running", { label }), durationMs: 2000 });
      try {
        await runAction(runtime.queue, action, input);
        toast.show({ kind: "success", message: tx("toast.done", { label }) });
        return true;
      } catch (error: unknown) {
        toast.show({ kind: "error", message: errorMessage(error) });
        return false;
      }
    },
    [runtime, tx, errorMessage],
  );

  return useMemo<PaletteActions>(() => {
    const newSite = () => {
      goTo("sites");
      newSitePending = true;
      // After the route change the Sites screen is mounted (or mounting) and opens its form.
      window.setTimeout(() => {
        window.dispatchEvent(new CustomEvent(NEW_SITE_EVENT));
      }, 0);
    };
    const guarded = (method: string, label: string, go: () => Promise<boolean>) => {
      const capability = capabilities.can(method);
      if (!capability.available) {
        const reason =
          capability.text === null
            ? label
            : translateError(capability.text.key, capability.text.values);
        toast.show({ kind: "error", message: `${label}. ${tx("unavailable", { reason })}` });
        return Promise.resolve(false);
      }
      return go();
    };
    const api: PaletteActions = {
      goTo,
      newSite,
      run,
      startAll: (label) =>
        guarded("services.start", label, () => run(startAllServices, undefined, label)),
      stopAll: (label) =>
        guarded("services.stop", label, () => run(stopAllServices, undefined, label)),
      openTerminal: (label) =>
        guarded("terminal.open", label, () => run(openTerminal, undefined, label)),
      handlers: () => {
        const handlers: ShortcutHandlers = {
          "palette.open": onTogglePalette,
          "services.startAll": () => void api.startAll(tx("commands.startAll")),
          "services.stopAll": () => void api.stopAll(tx("commands.stopAll")),
          "sites.new": newSite,
          "logs.open": () => {
            goTo("logs");
          },
          "settings.open": () => {
            goTo("settings");
          },
          "terminal.open": () => void api.openTerminal(tx("commands.openTerminal")),
          "terminal.global": () => void api.openTerminal(tx("commands.openTerminal")),
        };
        for (const def of DEFAULT_SHORTCUTS) {
          if (!def.id.startsWith("nav.")) continue;
          const feature = def.id.slice("nav.".length) as FeatureId;
          handlers[def.id] = () => {
            goTo(feature);
          };
        }
        return handlers;
      },
    };
    return api;
  }, [goTo, run, capabilities, tx, translateError, onTogglePalette]);
}
