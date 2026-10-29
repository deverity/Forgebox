// Slot registries (M2-E1-T10, docs/07 section 4): later epics mount into the shell without
// editing shell files. A feature drops `src/features/<name>/slots.ts` (picked up by the glob in
// `features.ts`) or calls a `register*` function from its own module; the host screens
// (Site detail, Dashboard, Atenção, Settings) render `useSlot(registry)`.
//
// Registries are module-level singletons: registration is a side effect of loading a feature
// module and returns the function that undoes it (tests, hot reload). Entries are ordered by
// `order` (default 100) and then by `id`, so the result never depends on import order.
import { useSyncExternalStore, type ComponentType } from "react";
import type { I18nText } from "../ipc/describeError";

export interface SlotEntry {
  /** Unique within its registry; registering the same id again replaces the entry. */
  id: string;
  /** Lower first (default {@link DEFAULT_ORDER}). */
  order?: number;
}

export const DEFAULT_ORDER = 100;

export class SlotRegistry<E extends SlotEntry> {
  readonly name: string;
  private entries: readonly E[] = [];
  private readonly listeners = new Set<() => void>();

  constructor(name: string) {
    this.name = name;
  }

  /** Adds or replaces `entry`; returns the function that removes it again. */
  register = (entry: E): (() => void) => {
    this.entries = sorted([...this.entries.filter((e) => e.id !== entry.id), entry]);
    this.emit();
    return () => {
      // Only removes the entry it registered (a later replacement stays).
      if (!this.entries.includes(entry)) return;
      this.entries = this.entries.filter((e) => e !== entry);
      this.emit();
    };
  };

  /** Snapshot, stable until the registry changes (safe for `useSyncExternalStore`). */
  list = (): readonly E[] => this.entries;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** Test hook: forgets every entry. */
  clear(): void {
    this.entries = [];
    this.emit();
  }

  private emit(): void {
    for (const listener of [...this.listeners]) listener();
  }
}

function sorted<E extends SlotEntry>(entries: E[]): readonly E[] {
  return entries.sort(
    (a, b) => (a.order ?? DEFAULT_ORDER) - (b.order ?? DEFAULT_ORDER) || (a.id < b.id ? -1 : 1),
  );
}

/** The entries of a registry, re-rendering when a feature registers or unregisters. */
export function useSlot<E extends SlotEntry>(registry: SlotRegistry<E>): readonly E[] {
  return useSyncExternalStore(registry.subscribe, registry.list, registry.list);
}

/** A label: an i18n key of `ns` (resolved with `t(key, {ns})`), so features own their texts. */
export interface SlotLabel {
  ns: string;
  key: string;
}

// --- site detail tabs (docs/07 section 5.4) -------------------------------------------------

export interface SiteTabProps {
  siteId: string;
}

export interface SiteTabSlot extends SlotEntry {
  label: SlotLabel;
  component: ComponentType<SiteTabProps>;
}

export const siteTabs = new SlotRegistry<SiteTabSlot>("site-tabs");
export const registerSiteTab = siteTabs.register;

// --- dashboard sections (docs/07 section 5.2) -----------------------------------------------

export interface DashboardSectionSlot extends SlotEntry {
  /** Accessible name of the section (a heading is the section's own business). */
  label: SlotLabel;
  component: ComponentType;
}

export const dashboardSections = new SlotRegistry<DashboardSectionSlot>("dashboard-sections");
export const registerDashboardSection = dashboardSections.register;

// --- Atenção sources (docs/07 section 5.2) --------------------------------------------------

export type AttentionSeverity = "info" | "warning" | "critical";

/** One thing that needs the user's attention: cause, action, equivalent command. */
export interface AttentionItem {
  /** Stable within the source (React key, dismissals). */
  id: string;
  severity: AttentionSeverity;
  /** `errors`-namespace texts, like every other cause shown by the GUI. */
  title: I18nText;
  cause?: I18nText;
  /** Equivalent `forge` command, never translated. */
  cli?: string;
  /** Where the primary action goes (router path), when it is a navigation. */
  href?: string;
}

/**
 * A source is a React hook: it reads the stores/queries it needs and returns its items. The
 * Dashboard renders one `<Host source>` component per registered source (a hook per
 * component, so a source added later never changes the number of hooks of a render).
 */
export interface AttentionSource extends SlotEntry {
  useItems: () => readonly AttentionItem[];
}

export const attentionSources = new SlotRegistry<AttentionSource>("attention-sources");
export const registerAttentionSource = attentionSources.register;

// --- settings sections (docs/07 section 5.11) ----------------------------------------------

export interface SettingsSectionSlot extends SlotEntry {
  label: SlotLabel;
  component: ComponentType;
}

export const settingsSections = new SlotRegistry<SettingsSectionSlot>("settings-sections");
export const registerSettingsSection = settingsSections.register;
