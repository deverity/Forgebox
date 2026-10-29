// Acceptance of M2-E1-T10: sidebar counts, collapse, daemon-down routes, slots, banners, gate.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { CLIENT_COMMANDS } from "../../ipc/describeError";
import { IDS } from "../../state/testFixtures";
import { FEATURES } from "../features";
import {
  attentionSources,
  dashboardSections,
  registerAttentionSource,
  registerDashboardSection,
  registerSettingsSection,
  registerSiteTab,
  settingsSections,
  siteTabs,
} from "../slots";
import { renderApp } from "../testApp";

afterEach(() => {
  siteTabs.clear();
  dashboardSections.clear();
  attentionSources.clear();
  settingsSections.clear();
});

describe("sidebar", () => {
  it("updates its counts on a service event without remounting", async () => {
    const h = await renderApp();
    const services = await screen.findByRole("link", { name: /^Services/ });
    expect(services).toHaveAccessibleName("Services, 1 running, 1 stopped");
    const sidebar = screen.getByTestId("sidebar");

    act(() => {
      h.fake.publish("service.running", {
        v: 1,
        instance_id: IDS.redis,
        name: "redis/default",
        tool: "redis",
        state: "running",
        pid: 77,
        strategy: null,
        exit_code: null,
        error: null,
      });
      h.fake.flush();
    });
    await waitFor(() => {
      expect(services).toHaveAccessibleName("Services, 2 running, 0 stopped");
    });
    // Same DOM nodes: updated in place, not remounted.
    expect(screen.getByRole("link", { name: /^Services/ })).toBe(services);
    expect(screen.getByTestId("sidebar")).toBe(sidebar);
    expect(screen.getByRole("link", { name: /^Sites/ })).toHaveAccessibleName("Sites, 1 site");
  });

  it("shows the catalog update count from tool.updates_available", async () => {
    const h = await renderApp();
    expect(screen.queryByTestId("badge-catalog")).toBeNull();
    act(() => {
      h.fake.publish("tool.updates_available", {
        v: 1,
        count: 2,
        updates: [
          { tool: "php", series: "8.3", from: "8.3.36", to: "8.3.37" },
          { tool: "mysql", series: "8.4", from: "8.4.2", to: "8.4.3" },
        ],
      });
      h.fake.flush();
    });
    expect(await screen.findByRole("link", { name: /^Catalog/ })).toHaveAccessibleName(
      "Catalog, 2 updates available",
    );
  });

  const MAIL_STATUS = {
    "mail.status": () => ({
      running: true,
      instance_id: null,
      smtp_port: 1025,
      ui_url: null,
      messages: 5,
      unread: 3,
    }),
  };

  it("hides the mail badge while mail.status is not implemented", async () => {
    // The default fake daemon answers mail.status with `not_implemented` (hello says so).
    await renderApp({ extra: { handlers: MAIL_STATUS } });
    expect(screen.getByRole("link", { name: "Mail" })).toBeInTheDocument();
    expect(screen.queryByTestId("badge-mail")).toBeNull();
    expect(screen.queryByText("3")).toBeNull();
  });

  it("shows the unread count once mail.status exists", async () => {
    await renderApp({ extra: { notImplemented: [], handlers: MAIL_STATUS } });
    expect(
      await screen.findByRole("link", { name: "Mail, 3 unread messages" }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("badge-mail")).toHaveTextContent("3");
  });

  it("lists the screens in order, Settings below, Diagnostics out of the sidebar", async () => {
    await renderApp();
    const nav = screen.getByRole("navigation", { name: "Main navigation" });
    const paths = within(nav)
      .getAllByRole("link")
      .map((a) => a.getAttribute("href"));
    expect(paths).toEqual([
      "/dashboard",
      "/services",
      "/sites",
      "/php",
      "/databases",
      "/mail",
      "/logs",
      "/catalog",
      "/terminal",
      "/settings",
    ]);
    expect(within(nav).queryByRole("link", { name: /Diagnostics/ })).toBeNull();
    expect(screen.getByRole("link", { name: "Dashboard" })).toHaveAttribute("aria-current", "page");
  });

  it("collapses to icons below 1100 px with aria-labels and tooltips", async () => {
    const h = await renderApp({ width: 1000 });
    const sidebar = screen.getByTestId("sidebar");
    expect(sidebar).toHaveAttribute("data-collapsed", "true");
    // No visible text: the accessible name comes from aria-label (counts included).
    const services = screen.getByRole("link", { name: "Services, 1 running, 1 stopped" });
    expect(services).toHaveAttribute("aria-label");
    expect(within(sidebar).queryByText("Services")).toBeNull();
    expect(screen.getByRole("link", { name: "Dashboard" })).toHaveAttribute(
      "aria-label",
      "Dashboard",
    );
    expect(screen.getByRole("button", { name: "Start all" })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Context: Local" })).toBeInTheDocument();

    // Native tooltips (title), like the window controls: no Radix in the first paint.
    expect(screen.getByRole("link", { name: "Dashboard" })).toHaveAttribute("title", "Dashboard");
    expect(services).toHaveAttribute("title", "Services · 1 running, 1 stopped");
    expect(screen.getByRole("button", { name: "Start all" }).parentElement).toHaveAttribute(
      "title",
      "Start all",
    );

    h.resize(1400);
    expect(sidebar).toHaveAttribute("data-collapsed", "false");
    expect(within(sidebar).getByText("Services")).toBeInTheDocument();
    h.resize(1099);
    expect(sidebar).toHaveAttribute("data-collapsed", "true");
  });

  it("offers Local and a disabled 'Add remote (M6)' context", async () => {
    await renderApp();
    // The lazy select shows a plain box with the same name until its chunk arrives.
    expect(screen.getByTestId("context-selector")).toHaveTextContent("Local");
    const trigger = await screen.findByRole("combobox", { name: "Context" });
    expect(trigger).toHaveTextContent("Local");
    await userEvent.click(trigger);
    const add = await screen.findByRole("option", { name: "Add remote (M6)" });
    expect(add).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByRole("option", { name: "Local" })).toBeInTheDocument();
  });

  it("shows the version and the updater state in the footer", async () => {
    await renderApp();
    expect(screen.getByTestId("sidebar-version")).toHaveTextContent(
      /^v\S+ · App updates arrive in 1\.0$/,
    );
  });

  it("Start all calls services.start with no targets", async () => {
    const h = await renderApp();
    await userEvent.click(screen.getByRole("button", { name: "Start all" }));
    await waitFor(() => {
      expect(h.fake.calls.some((c) => c.method === "services.start")).toBe(true);
    });
    expect(h.fake.calls.find((c) => c.method === "services.start")?.params).toEqual({});
  });
});

describe("status bar", () => {
  it("shows the connection, CPU · RAM, DNS and CA", async () => {
    await renderApp();
    const bar = screen.getByRole("contentinfo", { name: "Status bar" });
    expect(within(bar).getByText("Daemon connected")).toBeInTheDocument();
    expect(screen.getByTestId("status-resources")).toHaveTextContent("CPU");
    expect(screen.getByTestId("status-dns")).toHaveTextContent("DNS off");
    expect(screen.getByTestId("status-ca")).toHaveTextContent("CA ok");
    expect(screen.getByTestId("status-ca")).toHaveAttribute("href", "/diagnostics");
  });
});

describe("daemon down", () => {
  const PATHS = [
    ...FEATURES.map((f) => f.path),
    "/sites/01JSITE00000000000000000LJA",
    "/catalog/php",
  ];

  it.each(PATHS)("renders %s with actions disabled and explained", async (path) => {
    await renderApp({ scenario: "daemon-down", path });
    const feature = FEATURES.find((f) => path === f.path || path.startsWith(`${f.path}/`));
    expect(await screen.findByTestId(`page-${feature?.id ?? ""}`)).toBeInTheDocument();

    const banner = screen.getByTestId("connection-banner");
    expect(banner).toHaveTextContent("The daemon is stopped");
    expect(within(banner).getByText(CLIENT_COMMANDS.start_daemon)).toBeInTheDocument();
    expect(within(banner).getByRole("button", { name: "Start" })).toBeEnabled();

    // The onboarding screen has no sidebar; everywhere else the action is disabled and explained.
    if (path !== "/onboarding") {
      const startAll = screen.getByRole("button", { name: "Start all" });
      expect(startAll).toBeDisabled();
      const reason = startAll.getAttribute("aria-describedby") ?? "";
      expect(document.getElementById(reason)).toHaveTextContent("Not connected to the daemon.");
    }
    if (path !== "/onboarding") {
      expect(screen.getByTestId("status-connection")).toHaveTextContent("Daemon stopped");
    }
  });

  it("never shows a spinner without text", async () => {
    const h = await renderApp({ scenario: "daemon-down", path: "/services" });
    for (const status of ["connecting", "starting", "reconnecting"] as const) {
      act(() => {
        h.fake.setStatus(status);
      });
      const banner = await screen.findByTestId("connection-banner");
      await waitFor(() => {
        expect(banner.dataset.status).toBe(status);
      });
      const spinners = banner.querySelectorAll(".animate-spin");
      expect(spinners.length).toBeGreaterThan(0);
      for (const spinner of spinners) {
        expect(spinner.parentElement?.textContent.trim().length).toBeGreaterThan(0);
      }
    }
    for (const spinner of document.querySelectorAll(".animate-spin")) {
      expect(spinner.parentElement?.textContent.trim().length).toBeGreaterThan(0);
    }
  });

  it("'Start' asks the bridge to start the daemon and the banner goes away", async () => {
    await renderApp({ scenario: "daemon-down" });
    await userEvent.click(screen.getByRole("button", { name: "Start" }));
    await waitFor(() => {
      expect(screen.queryByTestId("connection-banner")).toBeNull();
    });
  });
});

describe("connection banners", () => {
  it.each([
    ["starting", "Starting the daemon…"],
    ["reconnecting", "Reconnecting to the daemon…"],
  ] as const)("%s", async (status, text) => {
    const h = await renderApp();
    expect(screen.queryByTestId("connection-banner")).toBeNull();
    act(() => {
      h.fake.setStatus(status);
    });
    expect(await screen.findByTestId("connection-banner")).toHaveTextContent(text);
    // The whole app stays navigable.
    await userEvent.click(screen.getByRole("link", { name: /^Logs/ }));
    expect(await screen.findByTestId("page-logs")).toBeInTheDocument();
  });
});

describe("routes", () => {
  it("redirects / to the Dashboard and each placeholder names its task", async () => {
    const h = await renderApp({ path: "/" });
    expect(await screen.findByTestId("page-dashboard")).toHaveTextContent("M2-E2-T14");
    expect(h.router.state.location.pathname).toBe("/dashboard");
    const nav = screen.getByRole("navigation", { name: "Main navigation" });
    const links = within(nav).getAllByRole("link");
    expect(links).toHaveLength(FEATURES.filter((f) => f.nav !== "none").length);
    for (const f of FEATURES.filter((x) => x.nav !== "none")) {
      const link = links.find((a) => a.getAttribute("href") === f.path);
      if (link === undefined) throw new Error(`no sidebar link for ${f.path}`);
      await userEvent.click(link);
      expect(await screen.findByTestId(`page-${f.id}`)).toHaveTextContent(f.task);
    }
  });

  it("opens /sites/:id with the param", async () => {
    await renderApp({ path: "/sites/abc" });
    expect(await screen.findByText("Site abc")).toBeInTheDocument();
  });

  it("routes /diagnostics from the status bar chips", async () => {
    await renderApp();
    await userEvent.click(screen.getByTestId("status-dns"));
    expect(await screen.findByTestId("page-diagnostics")).toHaveTextContent("M2-E3-T19");
  });

  it("shows a not-found page with a way home", async () => {
    await renderApp({ path: "/nope" });
    expect(await screen.findByText("This page does not exist.")).toBeInTheDocument();
  });

  it("sends an uninitialized root to onboarding, without the sidebar", async () => {
    const h = await renderApp({ scenario: "uninitialized", path: "/services" });
    expect(await screen.findByTestId("page-onboarding")).toBeInTheDocument();
    expect(h.router.state.location.pathname).toBe("/onboarding");
    expect(screen.queryByTestId("sidebar")).toBeNull();
  });

  it("keeps the gallery reachable outside production", async () => {
    await renderApp({ path: "/__gallery" });
    expect(
      await screen.findByRole("heading", { level: 1, name: "Component gallery" }),
    ).toBeInTheDocument();
  });
});

describe("slot registries", () => {
  // A feature module registering itself, as `src/features/<name>/slots.ts` does.
  function featureModule() {
    const offTab = registerSiteTab({
      id: "perf",
      label: { ns: "shell", key: "nav.logs" },
      component: ({ siteId }) => <p>Performance of {siteId}</p>,
    });
    const offSection = registerDashboardSection({
      id: "projects",
      label: { ns: "shell", key: "nav.sites" },
      component: () => <p>Projects section</p>,
    });
    return () => {
      offTab();
      offSection();
    };
  }

  it("renders a registered site tab and a dashboard section", async () => {
    const unregister = featureModule();
    await renderApp({ path: "/sites/abc" });
    expect(await screen.findByRole("tab", { name: "Logs" })).toBeInTheDocument();
    expect(screen.getByText("Performance of abc")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("link", { name: "Dashboard" }));
    expect(await screen.findByText("Projects section")).toBeInTheDocument();
    expect(screen.getByTestId("dashboard-section-projects")).toBeInTheDocument();

    // Registered later: rendered without remounting the screen.
    act(() => {
      registerDashboardSection({
        id: "late",
        order: 1,
        label: { ns: "shell", key: "nav.php" },
        component: () => <p>Late section</p>,
      });
    });
    expect(await screen.findByText("Late section")).toBeInTheDocument();
    unregister();
    await waitFor(() => {
      expect(screen.queryByText("Projects section")).toBeNull();
    });
  });

  it("contains a failing slot component", async () => {
    const original = console.error;
    console.error = () => undefined;
    try {
      registerDashboardSection({
        id: "broken",
        label: { ns: "shell", key: "nav.php" },
        component: () => {
          throw new Error("boom");
        },
      });
      registerDashboardSection({
        id: "fine",
        label: { ns: "shell", key: "nav.php" },
        component: () => <p>Fine</p>,
      });
      await renderApp();
      expect(await screen.findByText("Fine")).toBeInTheDocument();
    } finally {
      console.error = original;
    }
  });

  it("keeps Atenção sources and settings sections registered for later epics", () => {
    registerAttentionSource({ id: "ports", useItems: () => [] });
    registerSettingsSection({
      id: "general",
      label: { ns: "shell", key: "nav.settings" },
      component: () => null,
    });
    expect(attentionSources.list().map((s) => s.id)).toEqual(["ports"]);
    expect(settingsSections.list().map((s) => s.id)).toEqual(["general"]);
  });
});
