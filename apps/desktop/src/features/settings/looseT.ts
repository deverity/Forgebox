import type { TFunction } from "i18next";
import { useForgeQuery } from "../../state";

export type LooseT = (key: string, values?: Record<string, unknown>) => string;

/**
 * `t` for keys built at run time (`rows.<key>.label`, `shortcuts.names.<id>`): the typed
 * resources only know literal keys, and a missing translation falls back to `defaultValue`.
 */
export function loose(t: TFunction<"settings"> | TFunction<"palette">): LooseT {
  return t as unknown as LooseT;
}

/** Root folder of the installation, from `daemon.status` (empty until it loads). */
export function useRoot(): string {
  return useForgeQuery("daemon.status", {}).data?.root ?? "";
}
