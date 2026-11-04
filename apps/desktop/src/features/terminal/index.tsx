// Terminal screen (docs/07 §5.10, M2-E3-T15): tabs over sessions hosted by the daemon. The tabs
// live in `state/terminals.ts` (so the shortcuts, the palette and the tray can open one before
// this chunk loads); the emulators live in `registry.ts` and survive navigation.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { CliCommand, EmptyState, IconButton, Select } from "../../design/primitives";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { LOCAL_CONTEXT } from "../../ipc/bridge";
import { useCapability, useConnectionStatus, useHello, useRuntime, useSites } from "../../state";
import {
  activateTab,
  openTerminal,
  reattach,
  useTerminals,
  type TerminalTab,
} from "../../state/terminals";
import { Glyph } from "./Glyph";
import { TerminalPane } from "./TerminalPane";
import { closeEntry } from "./registry";

/** Tab label: the title, with the context name appended when the daemon is not this machine's. */
export function tabLabel(
  tab: TerminalTab,
  fallback: string,
  context: string,
  remote: boolean,
  withContext: (title: string, context: string) => string,
): string {
  const title = tab.title === "" ? fallback : tab.title;
  return remote ? withContext(title, context) : title;
}

export function TerminalPage() {
  const { t } = useTranslation("terminal");
  const runtime = useRuntime();
  const hello = useHello();
  const status = useConnectionStatus();
  const capability = useCapability("terminal.open");
  const sites = useSites();
  const allTabs = useTerminals((s) => s.tabs);
  const activeKey = useTerminals((s) => s.activeKey);
  const tabs = useMemo(
    () => allTabs.filter((tab) => tab.context === runtime.context),
    [allTabs, runtime.context],
  );
  const remote = hello !== null && !hello.local;
  const contextName = runtime.context === LOCAL_CONTEXT ? "local" : runtime.context;
  const [siteChoice, setSiteChoice] = useState("");

  // Reattach: sessions the daemon still hosts (reload, reconnect) come back as tabs whose
  // streams resume from the `last_seq` listed.
  const connected = status === "connected" && capability.available;
  const session = hello?.session_id ?? null;
  useEffect(() => {
    if (!connected) return;
    void reattach(runtime.api).catch(() => undefined);
  }, [connected, session, runtime.api]);

  const deps = useMemo(() => ({ api: runtime.api, queue: runtime.queue }), [runtime]);
  const withContext = (title: string, context: string) =>
    t("withContext", { title, name: context });
  const active = tabs.find((tab) => tab.key === activeKey) ?? tabs.at(-1) ?? null;
  const siteList = Object.values(sites).sort((a, b) => a.host.localeCompare(b.host));

  return (
    <div data-testid="page-terminal" className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 px-4 pt-3">
        <h1 className="text-title-1">
          {remote ? withContext(t("title"), contextName) : t("title")}
        </h1>
      </div>
      <div className="flex items-center gap-2 px-4 py-2">
        {/* A group of buttons, not role=tablist: a tablist may only own tabs, and each tab here sits
            next to its own close button (axe aria-required-children). The current tab is
            marked with aria-current. */}
        <div role="group" aria-label={t("tabs.label")} className="flex min-w-0 flex-1 gap-1">
          {tabs.map((tab) => {
            const label = tabLabel(tab, t("tabs.global"), contextName, remote, withContext);
            const selected = tab.key === active?.key;
            return (
              <div
                key={tab.key}
                className={
                  selected
                    ? "flex items-center rounded-control border border-stroke-strong bg-surface"
                    : "flex items-center rounded-control border border-transparent hover:bg-surface-2"
                }
              >
                <button
                  type="button"
                  aria-current={selected ? "true" : undefined}
                  data-terminal-tab={tab.key}
                  className={cx(
                    "max-w-48 truncate rounded-control px-3 py-1 text-callout text-fg-primary",
                    FOCUS_RING,
                  )}
                  onClick={() => {
                    activateTab(tab.key);
                  }}
                >
                  {label}
                </button>
                <IconButton
                  label={t("tabs.close", { title: label })}
                  icon={<Glyph name="close" />}
                  onClick={() => {
                    closeEntry(tab.key, deps);
                  }}
                />
              </div>
            );
          })}
        </div>
        <IconButton
          label={t("tabs.new")}
          icon={<Glyph name="plus" />}
          disabled={!capability.available}
          onClick={() => {
            openTerminal({}, runtime.context);
          }}
        />
        <Select
          label={t("tabs.newSite")}
          width="sm"
          value={siteChoice}
          placeholder={t("tabs.chooseSite")}
          disabled={!capability.available || siteList.length === 0}
          options={siteList.map((site) => ({
            value: site.id,
            label: t("tabs.site", { host: site.host }),
          }))}
          onValueChange={(id) => {
            const site = siteList.find((s) => s.id === id);
            // Back to the placeholder: the picker is an action, not a stored choice.
            setSiteChoice("");
            if (site)
              openTerminal(
                { site: site.id, siteHost: site.host, title: t("tabs.site", { host: site.host }) },
                runtime.context,
              );
          }}
        />
      </div>
      <div className="relative min-h-0 flex-1 border-t border-stroke">
        {tabs.length === 0 ? (
          <EmptyState
            title={t("empty.title")}
            description={capability.available ? t("empty.description") : t("unavailable")}
          />
        ) : null}
        {tabs.map((tab) => (
          <TerminalPane key={tab.key} tab={tab} active={tab.key === active?.key} deps={deps} />
        ))}
      </div>
      <div className="flex items-center gap-3 border-t border-stroke px-4 py-2">
        <span className="text-caption text-fg-tertiary">{t("copyHint")}</span>
        <span className="ml-auto text-caption text-fg-tertiary">{t("cli")}</span>
        <CliCommand command="forge shell" />
      </div>
    </div>
  );
}
