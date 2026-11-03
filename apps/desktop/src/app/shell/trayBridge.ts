// The native tray and the webview (docs/07 section 7, M2-E1-T15): the tray asks the window to
// show a route ("Ver logs", "Configurações"...), and the webview tells the tray whether motion is
// reduced (the "starting" ring then stays on one frame). The Tauri APIs are imported lazily so
// they stay out of the initial bundle; outside Tauri everything is a no-op.
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import { REDUCED_MOTION_QUERY } from "../../design/motion";
import { isTauri } from "../appearance";

/** Event the tray emits with the route to show (`src-tauri/src/tray/menu.rs`). */
export const TRAY_NAVIGATE_EVENT = "forge://navigate";
/** Command that reports `prefers-reduced-motion` to the tray. */
export const TRAY_MOTION_COMMAND = "tray_motion";

/** Routes the tray may open: anything else in the payload is ignored. */
export const TRAY_ROUTES = ["/services", "/logs", "/mail", "/settings"] as const;
export type TrayRoute = (typeof TRAY_ROUTES)[number];

/** The route of a navigate event, or `null` when it is not one the tray opens. */
export function trayRoute(payload: unknown): TrayRoute | null {
  return TRAY_ROUTES.find((route) => route === payload) ?? null;
}

export interface TrayHost {
  /** Subscribes to the navigate event; resolves to the unsubscribe. */
  onNavigate(handler: (payload: unknown) => void): Promise<() => void>;
  reportMotion(reduced: boolean): Promise<void>;
}

export const tauriTrayHost: TrayHost = {
  async onNavigate(handler) {
    const { listen } = await import("@tauri-apps/api/event");
    return listen(TRAY_NAVIGATE_EVENT, (event) => {
      handler(event.payload);
    });
  },
  async reportMotion(reduced) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke(TRAY_MOTION_COMMAND, { reduced });
  },
};

/** Test hook: replaces the Tauri side. */
export const trayHostOverride: { host: TrayHost | null } = { host: null };

/** Mounted once by the shell. */
export function useTrayBridge() {
  const navigate = useNavigate();
  const navigateRef = useRef(navigate);
  useEffect(() => {
    navigateRef.current = navigate;
  });
  const host = trayHostOverride.host ?? (isTauri() ? tauriTrayHost : null);

  useEffect(() => {
    if (host === null) return;
    let off: (() => void) | undefined;
    let disposed = false;
    void host
      .onNavigate((payload) => {
        const route = trayRoute(payload);
        if (route !== null) void navigateRef.current({ to: route });
      })
      .then((unlisten) => {
        if (disposed) unlisten();
        else off = unlisten;
      })
      .catch((error: unknown) => {
        console.warn("tray: navigate listener not attached", error);
      });
    return () => {
      disposed = true;
      off?.();
    };
  }, [host]);

  useEffect(() => {
    if (host === null || typeof window.matchMedia !== "function") return;
    const query = window.matchMedia(REDUCED_MOTION_QUERY);
    const report = () => {
      host.reportMotion(query.matches).catch((error: unknown) => {
        console.warn("tray: motion preference not reported", error);
      });
    };
    report();
    query.addEventListener("change", report);
    return () => {
      query.removeEventListener("change", report);
    };
  }, [host]);
}
