import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { cx, FOCUS_RING } from "../../design/primitives/shared";
import { useFormatters } from "../../design/primitives/useFormatters";
import { useHello } from "../../state";
import { FEATURES, NAV_GROUPS, type FeatureDef, type FeatureId, type NavGroup } from "../features";
import { ContextSelector } from "./ContextSelector";
import { Icon } from "./icons";
import { SidebarStatus } from "./SidebarStatus";
import { useMailUnread, useServiceCounts, useSiteCount, useUpdateCount } from "./useShellState";
import { useSidebarCollapsed } from "./useViewportCollapse";
import { useUpdaterSnapshot } from "../../features/updater/store";

// Glyphs of the services badge (the spoken form is the sr-only text next to it).
const DOT_ON = "●";
const DOT_OFF = "○";

/** What a nav row shows to the right of its label: a short visual and the full spoken text. */
interface NavBadge {
  visual: ReactNode;
  text: string;
}

function useNavBadges(): Partial<Record<FeatureId, NavBadge>> {
  const { t } = useTranslation("shell");
  const fmt = useFormatters();
  const { running, stopped } = useServiceCounts();
  const sites = useSiteCount();
  const updates = useUpdateCount();
  const unread = useMailUnread();
  const badges: Partial<Record<FeatureId, NavBadge>> = {};
  if (running + stopped > 0) {
    badges.services = {
      visual: (
        <>
          <span className="text-state-running">{DOT_ON}</span> {fmt.number(running)}
          <span className="text-state-stopped"> {DOT_OFF}</span> {fmt.number(stopped)}
        </>
      ),
      text: t("sidebar.servicesBadge", { running, stopped }),
    };
  }
  if (sites > 0) {
    badges.sites = { visual: fmt.number(sites), text: t("sidebar.sitesBadge", { count: sites }) };
  }
  if (updates > 0) {
    badges.catalog = {
      visual: `↑ ${fmt.number(updates)}`,
      text: t("sidebar.updatesBadge", { count: updates }),
    };
  }
  if (unread !== null) {
    badges.mail = {
      visual: fmt.number(unread),
      text: t("sidebar.mailBadge", { count: unread }),
    };
  }
  return badges;
}

// Finder-like row (M2-E1-T23): no borders, a 7 px rounded fill when selected, accent line icons.
const ROW = `relative flex h-7 items-center gap-2 rounded-item px-2 text-body text-fg-primary ${FOCUS_RING} hover:bg-fill aria-[current=page]:bg-fill-selected aria-[current=page]:font-semibold`;

function NavRow({
  feature,
  badge,
  collapsed,
}: {
  feature: FeatureDef;
  badge: NavBadge | undefined;
  collapsed: boolean;
}) {
  const { t } = useTranslation("shell");
  const label = t(`nav.${feature.id}`);
  const link = (
    <Link
      to={feature.path}
      className={cx(ROW, collapsed && "justify-center px-0")}
      // Collapsed: the visible text is gone, so the name (and its counts) is spelled out.
      {...(collapsed
        ? {
            "aria-label": badge === undefined ? label : `${label}, ${badge.text}`,
            // Native tooltip (like the window controls): no Radix in the first paint.
            title: badge === undefined ? label : `${label} · ${badge.text}`,
          }
        : {})}
    >
      {feature.icon === null ? null : (
        <span className="inline-flex shrink-0 text-accent">
          <Icon name={feature.icon} />
        </span>
      )}
      {collapsed ? (
        badge === undefined ? null : (
          <span
            aria-hidden="true"
            data-testid={`badge-${feature.id}`}
            className="absolute top-0.5 right-1 size-2 rounded-full bg-accent"
          />
        )
      ) : (
        <>
          <span className="min-w-0 flex-1 truncate">{label}</span>
          {badge === undefined ? null : (
            <>
              <span
                aria-hidden="true"
                data-testid={`badge-${feature.id}`}
                className="text-caption text-fg-secondary tabular-nums"
              >
                {badge.visual}
              </span>
              <span className="sr-only">, {badge.text}</span>
            </>
          )}
        </>
      )}
    </Link>
  );
  return <li>{link}</li>;
}

function Footer({ collapsed }: { collapsed: boolean }) {
  const { t } = useTranslation("shell");
  const hello = useHello();
  const update = useUpdaterSnapshot();
  const version =
    hello === null
      ? t("sidebar.versionUnknown")
      : t("sidebar.version", { version: hello.daemon_version });
  // Inactive (0.2.0 release): the fixed notice. Active: an announced update, or nothing extra.
  const updater = !update.active
    ? t("sidebar.updater")
    : update.available === null
      ? null
      : t("sidebar.updateAvailable", { version: update.available.version });
  const text = updater === null ? version : `${version} · ${updater}`;
  if (collapsed) {
    return (
      <span
        title={text}
        data-update={update.available === null ? undefined : "available"}
        className="mx-auto text-caption text-fg-secondary"
      >
        {version}
        {updater === null ? null : <span className="sr-only">{` · ${updater}`}</span>}
      </span>
    );
  }
  return (
    <p
      className="m-0 px-2 text-caption text-fg-secondary"
      data-testid="sidebar-version"
      data-update={update.available === null ? undefined : "available"}
    >
      {text}
    </p>
  );
}

/** One Finder-like group: an optional small grey heading over its rows. */
function NavGroupList({
  group,
  features,
  badges,
  collapsed,
}: {
  group: NavGroup;
  features: readonly FeatureDef[];
  badges: Partial<Record<FeatureId, NavBadge>>;
  collapsed: boolean;
}) {
  const { t } = useTranslation("shell");
  const headingId = `sidebar-group-${group}`;
  // `core` has no heading; collapsed, headings give way to a small gap.
  const heading = group === "core" || collapsed ? null : t(`sidebar.groups.${group}`);
  return (
    <div data-testid={`nav-group-${group}`} className="flex flex-col gap-0.5">
      {heading === null ? null : (
        <p id={headingId} className="m-0 px-2 pb-0.5 text-caption font-semibold text-fg-secondary">
          {heading}
        </p>
      )}
      <ul
        aria-labelledby={heading === null ? undefined : headingId}
        className="m-0 flex list-none flex-col gap-px p-0"
      >
        {features.map((f) => (
          <NavRow key={f.id} feature={f} badge={badges[f.id]} collapsed={collapsed} />
        ))}
      </ul>
    </div>
  );
}

export interface SidebarProps {
  /** Forces the mode (tests); by default the window width decides. */
  collapsed?: boolean;
}

/**
 * 220 px navigation column (56 px of icons below 1100 px) over the window material, running the
 * full window height from the top edge (docs/07 section 4, M2-E1-T23): the "Forgebox" brand
 * (also a drag region) where a titlebar used to be, the context selector, Finder-like groups,
 * and at the bottom Settings, the daemon/DNS/CA status and the version.
 */
export function Sidebar({ collapsed: forced }: SidebarProps) {
  const { t } = useTranslation("shell");
  const viewportCollapsed = useSidebarCollapsed();
  const collapsed = forced ?? viewportCollapsed;
  const badges = useNavBadges();
  const main = FEATURES.filter((f) => f.nav === "main");
  const footer = FEATURES.filter((f) => f.nav === "footer");
  return (
    <aside
      data-testid="sidebar"
      data-collapsed={collapsed}
      aria-label={t("sidebar.label")}
      className={cx(
        "flex h-full shrink-0 flex-col gap-3 border-r border-stroke bg-sidebar pb-2.5",
        collapsed ? "w-14 px-2" : "w-55 px-2.5",
      )}
    >
      {/* Where the titlebar used to be: the brand, and a handle to drag the window. */}
      <div
        data-tauri-drag-region
        data-testid="sidebar-brand"
        className={cx(
          "flex h-11 shrink-0 select-none items-center text-body font-semibold text-fg-primary",
          collapsed ? "justify-center" : "px-2",
        )}
      >
        {collapsed ? (
          <span data-tauri-drag-region aria-hidden="true">
            {t("title").slice(0, 1)}
          </span>
        ) : (
          <span data-tauri-drag-region>{t("title")}</span>
        )}
      </div>
      <ContextSelector collapsed={collapsed} />
      <nav aria-label={t("nav.label")} className="flex min-h-0 flex-1 flex-col gap-3">
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto">
          {NAV_GROUPS.map((group) => {
            const rows = main.filter((f) => (f.group ?? "core") === group);
            return rows.length === 0 ? null : (
              <NavGroupList
                key={group}
                group={group}
                features={rows}
                badges={badges}
                collapsed={collapsed}
              />
            );
          })}
        </div>
        <ul className="m-0 flex list-none flex-col gap-px p-0">
          {footer.map((f) => (
            <NavRow key={f.id} feature={f} badge={badges[f.id]} collapsed={collapsed} />
          ))}
        </ul>
      </nav>
      <SidebarStatus collapsed={collapsed} />
      <Footer collapsed={collapsed} />
    </aside>
  );
}
