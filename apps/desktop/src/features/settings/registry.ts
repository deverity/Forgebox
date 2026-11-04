import type { ConfigDocumentDto } from "../../ipc/generated";

/** What every section receives. */
export interface SectionProps {
  /** Label search (empty: show everything). */
  query: string;
  /** `system.config_list`, once loaded (sections made of GUI prefs ignore it). */
  doc: ConfigDocumentDto | undefined;
}

export type SectionId =
  | "general"
  | "filesystem"
  | "network"
  | "security"
  | "updates"
  | "editor"
  | "registries"
  | "shortcuts"
  | "advanced";

/** Order of the sub-navigation (docs/07 section 5.11). */
export const SECTION_IDS: readonly SectionId[] = [
  "general",
  "filesystem",
  "network",
  "security",
  "updates",
  "editor",
  "registries",
  "shortcuts",
  "advanced",
];
