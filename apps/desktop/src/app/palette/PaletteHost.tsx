import { Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "../../design/primitives/toastStore";
import { isTauri } from "../appearance";
import { OPEN_PALETTE_EVENT } from "../shell/Titlebar";
import { useCapability } from "../../state";
import {
  GLOBAL_SHORTCUT_EVENT,
  syncGlobalShortcuts,
  useShortcutBindings,
  useShortcutRegistry,
  useShortcuts,
  type GlobalInvoke,
} from "../shortcuts/useShortcuts";
import { usePaletteActions } from "./actions";

// The palette (cmdk, Radix Dialog, the pt-BR + en-US search bundles) is its own chunk, loaded
// the first time it opens; the shortcuts themselves are eager and tiny.
const CommandPalette = lazy(async () => {
  const module = await import("./CommandPalette");
  return { default: module.CommandPalette };
});

type Translate = (key: string, values?: Record<string, string | number>) => string;

const tauriInvoke: GlobalInvoke = async (command, args) => {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke(command, args);
};

/** Test hook: replaces the Tauri command that registers the global shortcuts. */
export const globalShortcutBackend: { invoke: GlobalInvoke | null } = { invoke: null };

/**
 * Owns the palette and every in-app shortcut of docs/07 section 8, and plugs the global
 * terminal shortcuts. Mounted once by the shell.
 *
 * The host itself never suspends on the lazy `palette` namespace, so its listeners exist from the
 * shell's first commit: a shortcut or a click on the search button while the texts load runs when
 * they arrive instead of being lost. Only the parts that show text sit in a boundary.
 */
export function PaletteHost() {
  const [open, setOpen] = useState(false);
  const [mounted, setMounted] = useState(false);
  const registry = useShortcutRegistry();
  const toggle = useCallback(() => {
    setMounted(true);
    setOpen((value) => !value);
  }, []);
  const actions = usePaletteActions(toggle);
  const { ready } = useTranslation("palette", { useSuspense: false });

  // The saved overrides apply from the first interaction on (a failed read keeps the defaults).
  useEffect(() => {
    void registry.load();
  }, [registry]);

  useEffect(() => {
    const onOpen = () => {
      setMounted(true);
      setOpen(true);
    };
    window.addEventListener(OPEN_PALETTE_EVENT, onOpen);
    return () => {
      window.removeEventListener(OPEN_PALETTE_EVENT, onOpen);
    };
  }, []);

  useShortcuts(actions.handlers(), ready);

  return (
    <>
      <Suspense fallback={null}>
        <GlobalShortcuts openTerminal={actions.openTerminal} />
      </Suspense>
      {mounted ? (
        <Suspense fallback={null}>
          <CommandPalette open={open} onOpenChange={setOpen} actions={actions} />
        </Suspense>
      ) : null}
    </>
  );
}

/** Its failure toasts and label need the texts, so it starts once they are loaded. */
function GlobalShortcuts({ openTerminal }: { openTerminal: (label: string) => Promise<boolean> }) {
  useGlobalShortcuts(openTerminal);
  return null;
}

/**
 * System-wide Ctrl+Alt+T and Ctrl+` (docs/07 section 8): registered only while the daemon
 * implements `terminal.open`; a failure to register is reported once per attempt and is never
 * fatal. Outside Tauri there is nothing to register.
 */
function useGlobalShortcuts(openTerminal: (label: string) => Promise<boolean>) {
  const invokeOverride = globalShortcutBackend.invoke;
  const { t } = useTranslation("palette");
  const tx = t as unknown as Translate;
  const bindings = useShortcutBindings();
  const capability = useCapability("terminal.open");
  const implemented = capability.available;
  const enabledHere = invokeOverride !== null || isTauri();
  const openRef = useRef(openTerminal);
  useEffect(() => {
    openRef.current = openTerminal;
  });
  const label = tx("commands.openTerminal");

  useEffect(() => {
    if (!enabledHere) return;
    let cancelled = false;
    void syncGlobalShortcuts(invokeOverride ?? tauriInvoke, bindings, implemented).then(
      (report) => {
        if (cancelled) return;
        for (const failure of report.failed) {
          toast.show({
            kind: "error",
            message: tx("global.failed", { chord: failure.accelerator, error: failure.error }),
          });
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [enabledHere, invokeOverride, bindings, implemented, tx]);

  useEffect(() => {
    if (!enabledHere || !isTauri()) return;
    let off: (() => void) | undefined;
    let disposed = false;
    void import("@tauri-apps/api/event").then(async ({ listen }) => {
      const unlisten = await listen(GLOBAL_SHORTCUT_EVENT, () => {
        void openRef.current(label);
      });
      if (disposed) unlisten();
      else off = unlisten;
    });
    return () => {
      disposed = true;
      off?.();
    };
  }, [enabledHere, label]);
}
