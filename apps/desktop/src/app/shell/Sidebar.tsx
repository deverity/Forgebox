import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { cx, FOCUS_RING } from "../../design/primitives/shared";
import { useFormatters } from "../../design/primitives/useFormatters";
import { useHello } from "../../state";
import { FEATURES, type FeatureDef, type FeatureId } from "../features";
import { ContextSelector } from "./ContextSelector";
import { Icon } from "./icons";
import { StartAll } from "./StartAll";
import { useMailUnread, useServiceCounts, useSiteCount, useUpdateCount } from "./useShellState";
import { useSidebarCollapsed } from "./useViewportCollapse";

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

const ROW =
  `relative flex h-8 items-center gap-3 rounded-control px-3 text-body text-fg-primary ${FOCUS_RING} hover:bg-surface-2 aria-[current=page]:bg-surface-2 aria-[current=page]:font-semibold`;

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
      {feature.icon === null ? null : <Icon name={feature.icon} />}
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
  const version =
    hello === null
      ? t("sidebar.versionUnknown")
      : t("sidebar.version", { version: hello.daemon_version });
  if (collapsed) {
    return (
      <span
        title={`${version} · ${t("sidebar.updater")}`}
        className="mx-auto text-caption text-fg-tertiary"
      >
        {version}
        <span className="sr-only">{` · ${t("sidebar.updater")}`}</span>
      </span>
    );
  }
  return (
    <p className="px-3 text-caption text-fg-tertiary" data-testid="sidebar-version">
      {`${version} · ${t("sidebar.updater")}`}
    </p>
  );
}

export interface SidebarProps {
  /** Forces the mode (tests); by default the window width decides. */
  collapsed?: boolean;
}

/** 220 px navigation column (56 px of icons below 1100 px), drawn with the window material. */
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
        "flex shrink-0 flex-col gap-3 bg-sidebar py-2",
        collapsed ? "w-14 px-2" : "w-55 px-3",
      )}
    >
      <ContextSelector collapsed={collapsed} />
      <StartAll collapsed={collapsed} />
      <nav aria-label={t("nav.label")} className="flex min-h-0 flex-1 flex-col gap-2">
        <ul className="m-0 flex min-h-0 flex-1 list-none flex-col gap-0.5 overflow-y-auto p-0">
          {main.map((f) => (
            <NavRow key={f.id} feature={f} badge={badges[f.id]} collapsed={collapsed} />
          ))}
        </ul>
        <hr className="m-0 border-0 border-t border-stroke" />
        <ul className="m-0 flex list-none flex-col gap-0.5 p-0">
          {footer.map((f) => (
            <NavRow key={f.id} feature={f} badge={badges[f.id]} collapsed={collapsed} />
          ))}
        </ul>
      </nav>
      <Footer collapsed={collapsed} />
    </aside>
  );
}
