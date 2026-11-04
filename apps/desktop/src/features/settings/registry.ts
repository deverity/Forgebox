import type { ConfigDocumentDto } from "../../ipc/generated";

/** What every section receives. */
export interface SectionProps {
  /** Label search (empty: show everything). */
  query: string;
  /** `system.config_list`, once loaded (sections made of GUI prefs ignore it). */
  doc: ConfigDocumentDto | undefined;
}

export type SectionId = "general" | "network" | "security" | "editor" | "shortcuts" | "advanced";

/** Order of the sub-navigation. M2-E3-T18 adds File system, Updates, Registries... between these. */
export const SECTION_IDS: readonly SectionId[] = [
  "general",
  "network",
  "security",
  "editor",
  "shortcuts",
  "advanced",
];
