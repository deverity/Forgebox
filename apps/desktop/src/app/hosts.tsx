// Hosts for the slot registries: the minimal renderers the placeholder screens use, and that
// the real Dashboard (M2-E2-T14) and Site detail (M2-E2-T16) can reuse or replace. A slot
// component that throws is contained by its own error boundary: one broken feature never
// takes the screen down.
import { Tabs } from "radix-ui";
import { Component, Suspense, type ErrorInfo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { FOCUS_RING } from "../design/primitives/shared";
import { dashboardSections, siteTabs, useSlot, type SlotLabel } from "./slots";

class SlotBoundary extends Component<{ id: string; children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  override componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error(`slot ${this.props.id} failed`, error, info.componentStack);
  }
  override render() {
    return this.state.failed ? null : this.props.children;
  }
}

/** The text of a slot label from its feature's own namespace. */
export function SlotLabelText({ label }: { label: SlotLabel }) {
  const { t } = useTranslation(label.ns as "shell");
  // Keys of feature namespaces are not part of the typed Resources of the shell.
  return <>{(t as unknown as (key: string) => string)(label.key)}</>;
}

/** Dashboard sections in registry order, each in its own boundary. */
export function DashboardSectionsHost() {
  const sections = useSlot(dashboardSections);
  return (
    <>
      {sections.map((section) => (
        <SlotBoundary key={section.id} id={section.id}>
          <Suspense fallback={null}>
            <div data-testid={`dashboard-section-${section.id}`}>
              <section.component />
            </div>
          </Suspense>
        </SlotBoundary>
      ))}
    </>
  );
}

/** Tab strip and panels of the site detail from the registered tabs. */
export function SiteTabsHost({ siteId }: { siteId: string }) {
  const { t } = useTranslation("shell");
  const tabs = useSlot(siteTabs);
  const first = tabs[0];
  if (first === undefined) {
    return <p className="px-4 text-callout text-fg-secondary">{t("placeholder.noTabs")}</p>;
  }
  return (
    <Tabs.Root defaultValue={first.id} className="flex min-h-0 flex-1 flex-col">
      <Tabs.List
        aria-label={t("placeholder.siteTabs")}
        className="flex gap-1 border-b border-stroke px-4"
      >
        {tabs.map((tab) => (
          <Tabs.Trigger
            key={tab.id}
            value={tab.id}
            className={`h-8 px-3 text-body text-fg-secondary ${FOCUS_RING} data-[state=active]:font-semibold data-[state=active]:text-fg-primary`}
          >
            <Suspense fallback={null}>
              <SlotLabelText label={tab.label} />
            </Suspense>
          </Tabs.Trigger>
        ))}
      </Tabs.List>
      {tabs.map((tab) => (
        <Tabs.Content key={tab.id} value={tab.id} className="min-h-0 flex-1 overflow-y-auto p-4">
          <SlotBoundary id={tab.id}>
            <Suspense fallback={null}>
              <tab.component siteId={siteId} />
            </Suspense>
          </SlotBoundary>
        </Tabs.Content>
      ))}
    </Tabs.Root>
  );
}
