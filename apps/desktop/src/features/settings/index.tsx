// Settings (docs/07 section 5.11, M2-E3-T16/T17): sub-navigation by section, a label search and
// rows generated from the schema of `system.config_list`. Sections: Geral, Sistema de arquivos,
// Rede, Seguranca, Atualizacoes, Editor/Terminal, Registries, Atalhos and Avancado.
import { useState } from "react";
import type { ComponentType } from "react";
import { useTranslation } from "react-i18next";
import { DEFAULT_SHORTCUTS } from "../../app/shortcuts/registry";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { useForgeQuery } from "../../state";
import { loose } from "./looseT";
import { matchesSearch } from "./model";
import { SECTION_IDS, type SectionId, type SectionProps } from "./registry";
import { editorRows, rowLabel } from "./schemaRows";
import { ADVANCED_ROWS, AdvancedSection } from "./sections/advanced";
import { EditorSection } from "./sections/editor";
import { FILESYSTEM_ROWS, FilesystemSection } from "./sections/filesystem";
import { GENERAL_ROWS, GeneralSection } from "./sections/general";
import { NETWORK_ROWS, NetworkSection } from "./sections/network";
import { REGISTRIES_ROWS, RegistriesSection } from "./sections/registries";
import { SECURITY_ROWS, SecuritySection } from "./sections/security";
import { ShortcutsSection } from "./sections/shortcuts";
import { UPDATES_ROWS, UpdatesSection } from "./sections/updates";

const LIST_METHOD = "system.config_list";

const SECTION_VIEWS: Readonly<Record<SectionId, ComponentType<SectionProps>>> = {
  general: GeneralSection,
  filesystem: FilesystemSection,
  network: NetworkSection,
  security: SecuritySection,
  updates: UpdatesSection,
  editor: EditorSection,
  registries: RegistriesSection,
  shortcuts: ShortcutsSection,
  advanced: AdvancedSection,
};

const SEARCH_CLASS =
  "h-8 w-full rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent";

export function SettingsPage() {
  const { t } = useTranslation("settings");
  const { t: tp } = useTranslation("palette");
  const config = useForgeQuery("system.config_list", {});
  const doc = config.data;
  const [selected, setSelected] = useState<SectionId>("general");
  const [query, setQuery] = useState("");

  // Labels each section answers to in the search (its name plus the rows it holds).
  const labelsOf = (id: SectionId): string[] => {
    switch (id) {
      case "general":
        return GENERAL_ROWS.map((row) => loose(t)(`general.${row}.label`));
      case "filesystem":
        return FILESYSTEM_ROWS.map((row) => loose(t)(`filesystem.${row}.label`));
      case "updates":
        return UPDATES_ROWS.map((row) => loose(t)(`updates.${row}.label`));
      case "registries":
        return REGISTRIES_ROWS.map((row) => loose(t)(`registries.${row}.label`));
      case "network":
        return NETWORK_ROWS.map((row) => loose(t)(`network.${row}.label`));
      case "security":
        return SECURITY_ROWS.map((row) => loose(t)(`security.${row}.label`));
      case "editor":
        return editorRows(doc).flatMap((row) => [rowLabel(t, row), row.key]);
      case "shortcuts":
        return DEFAULT_SHORTCUTS.map((def) => loose(tp)(`shortcuts.names.${def.id}`));
      case "advanced":
        return ADVANCED_ROWS.map((row) => loose(t)(`advanced.${row}.label`));
    }
  };
  const visible = SECTION_IDS.filter((id) =>
    matchesSearch(query, t(`sections.${id}`), ...labelsOf(id)),
  );
  const active = visible.includes(selected) ? selected : visible[0];
  const View = active === undefined ? null : SECTION_VIEWS[active];
  // A match on the section name alone shows the whole section.
  const sectionQuery =
    active !== undefined && matchesSearch(query, t(`sections.${active}`)) ? "" : query;

  return (
    <div className="flex h-full min-h-0 flex-col" data-testid="page-settings">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <h1 className="text-title-1">{t("title")}</h1>
        <input
          type="search"
          aria-label={t("search.label")}
          placeholder={t("search.placeholder")}
          value={query}
          className={cx(SEARCH_CLASS, "max-w-72")}
          onChange={(event) => {
            setQuery(event.target.value);
          }}
        />
      </div>
      <div className="flex min-h-0 flex-1 gap-4 px-4 pb-4">
        <nav aria-label={t("nav.label")} className="flex w-44 shrink-0 flex-col gap-1">
          {visible.map((id) => (
            <button
              key={id}
              type="button"
              data-testid={`settings-nav-${id}`}
              aria-current={id === active ? "page" : undefined}
              className={cx(
                "rounded-control px-3 py-1.5 text-left text-body hover:bg-surface-2",
                id === active && "bg-surface-2 font-medium",
                FOCUS_RING,
              )}
              onClick={() => {
                setSelected(id);
              }}
            >
              {t(`sections.${id}`)}
            </button>
          ))}
        </nav>
        <div className="min-w-0 flex-1 overflow-y-auto">
          {config.error === null ? null : (
            <div data-testid="settings-load-error" className="mb-3">
              <ErrorNotice
                error={config.error}
                context={{ method: LIST_METHOD }}
                onRetry={() => {
                  void config.refetch();
                }}
              />
            </div>
          )}
          {View === null || active === undefined ? (
            <p data-testid="settings-no-match" className="text-body text-fg-secondary">
              {t("search.none", { query })}
            </p>
          ) : (
            <section aria-label={t(`sections.${active}`)}>
              <h2 className="pb-2 text-title">{t(`sections.${active}`)}</h2>
              <View query={sectionQuery} doc={doc} />
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
