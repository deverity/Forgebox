// Acceptance of M2-E2-T18: the catalog list (debounced search, filters, cards, stacks, lazy
// icons) and the tool/stack pages (versions, variant, install with the equivalent CLI, stale
// and error states, the plan total before a stack install).
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { catalogHandlers } from "../../../qa/fixtures/scenarios/catalog";
import { renderApp, type AppHarness } from "../../app/testApp";
import { populatedData } from "../../state/testFixtures";
import { resetIconCache } from "./CatalogIcon";
import { SEARCH_DEBOUNCE_MS } from "./CatalogPage";

vi.mock("../../ipc/native", () => ({
  copyText: () => Promise.resolve(true),
  openUrl: () => Promise.resolve(true),
  pickPath: () => Promise.resolve(null),
}));

let harness: AppHarness | null = null;
beforeEach(() => {
  resetIconCache();
});
afterEach(async () => {
  vi.unstubAllGlobals();
  await harness?.layer.dispose();
  harness = null;
});

async function open(path: string, extra: Parameters<typeof renderApp>[0] = {}) {
  const data = populatedData();
  harness = await renderApp({
    path,
    data,
    ...extra,
    extra: {
      notImplemented: [],
      handlers: catalogHandlers({ instances: [], sites: [], tools: [], daemon: data.daemon }),
      ...extra.extra,
    },
  });
  return harness;
}

const cards = () => [...document.querySelectorAll<HTMLElement>("[data-catalog-card]")];
const cardIds = () => cards().map((card) => card.dataset.catalogCard);

describe("Catalog list", () => {
  it("shows tool cards with badges, version, size and the changelog link, and stacks as large cards", async () => {
    await open("/catalog");
    await screen.findByRole("heading", { level: 1, name: "Catalog" });
    await waitFor(() => {
      expect(cards().length).toBeGreaterThan(5);
    });
    const stack = document.querySelector<HTMLElement>('[data-catalog-card="laravel"]');
    expect(stack?.dataset.kind).toBe("stack");
    const stackHeight = Number.parseInt(stack?.style.height ?? "0", 10);
    const toolHeight = Number.parseInt(
      document.querySelector<HTMLElement>('[data-catalog-card="php"]')?.style.height ?? "0",
      10,
    );
    expect(stackHeight).toBeGreaterThan(toolHeight);
    expect(screen.getByRole("heading", { level: 2, name: "Stacks" })).toBeInTheDocument();

    const php = within(
      document.querySelector<HTMLElement>('[data-catalog-card="php"]') ?? document.body,
    );
    expect(php.getByText("Official registry")).toBeInTheDocument();
    expect(php.getByText("Native Windows")).toBeInTheDocument();
    expect(php.getByText("Latest: 8.4.27")).toBeInTheDocument();
    expect(php.getByText("About 31 MB")).toBeInTheDocument();
    await waitFor(() => {
      expect(php.getByTestId("changelog-link")).toBeInTheDocument();
    });

    const shady = within(
      document.querySelector<HTMLElement>('[data-catalog-card="shady"]') ?? document.body,
    );
    expect(shady.getByText("Third-party registry: community")).toBeInTheDocument();
    expect(shady.queryByText("Official registry")).toBeNull();
  });

  it("debounces the search: one tools.search for a burst of keystrokes", async () => {
    const h = await open("/catalog");
    await waitFor(() => {
      expect(cards().length).toBeGreaterThan(5);
    });
    const search = screen.getByRole("searchbox", { name: "Search the catalog" });
    const before = h.fake.calls.filter(
      (call) =>
        call.method === "tools.search" && (call.params as { text?: string }).text === "mailp",
    ).length;
    fireEvent.change(search, { target: { value: "m" } });
    fireEvent.change(search, { target: { value: "mailp" } });
    await new Promise((resolve) => setTimeout(resolve, SEARCH_DEBOUNCE_MS / 2));
    expect(
      h.fake.calls.some(
        (call) => call.method === "tools.search" && (call.params as { text?: string }).text === "m",
      ),
    ).toBe(false);
    await waitFor(() => {
      expect(cardIds()).toEqual(["mailpit"]);
    });
    const after = h.fake.calls.filter(
      (call) =>
        call.method === "tools.search" && (call.params as { text?: string }).text === "mailp",
    ).length;
    expect(after - before).toBe(1);
  });

  it("filters installed and updates available", async () => {
    await open("/catalog");
    await waitFor(() => {
      expect(cards().length).toBeGreaterThan(5);
    });
    const user = userEvent.setup();
    await user.click(screen.getByRole("radio", { name: "Updates available" }));
    expect(cardIds()).toEqual(["mysql"]);
    await user.click(screen.getByRole("radio", { name: "Installed" }));
    expect(screen.getByText(/Nothing matches|The catalog is empty/)).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "All" }));
    expect(cards().length).toBeGreaterThan(5);
  });

  it("loads icons only for cards that scrolled into view", async () => {
    const observers: { callback: IntersectionObserverCallback; element: Element }[] = [];
    class FakeObserver {
      private readonly callback: IntersectionObserverCallback;
      constructor(callback: IntersectionObserverCallback) {
        this.callback = callback;
      }
      observe(element: Element) {
        observers.push({ callback: this.callback, element });
      }
      unobserve() {
        /* not needed */
      }
      disconnect() {
        /* not needed */
      }
    }
    vi.stubGlobal("IntersectionObserver", FakeObserver);
    const h = await open("/catalog");
    await waitFor(() => {
      expect(cards().length).toBeGreaterThan(5);
    });
    expect(h.fake.calls.filter((call) => call.method === "tools.icon")).toHaveLength(0);
    const target = observers.find(
      (o) => o.element.closest("[data-catalog-card]")?.getAttribute("data-catalog-card") === "php",
    );
    expect(target).toBeDefined();
    act(() => {
      target?.callback(
        [{ isIntersecting: true, target: target.element } as IntersectionObserverEntry],
        {} as IntersectionObserver,
      );
    });
    await waitFor(() => {
      expect(
        h.fake.calls.filter((call) => call.method === "tools.icon").map((c) => c.params),
      ).toEqual([{ tool: "php" }]);
    });
  });

  it("explains an unavailable catalog instead of an empty list", async () => {
    await open("/catalog", { extra: { notImplemented: ["tools.search"] } });
    expect(await screen.findByText("The catalog is not available")).toBeInTheDocument();
  });
});

describe("Tool page", () => {
  it("lists versions by series and installs with version and variant, showing the CLI", async () => {
    const h = await open("/catalog/php");
    await screen.findByRole("heading", { level: 1, name: "PHP" });
    await screen.findByText("Series 8.4 (2 versions)");
    expect(screen.getByText("Series 8.3 (2 versions)")).toBeInTheDocument();
    // The safe Markdown description: raw HTML is text.
    expect(screen.getByTestId("markdown").textContent).toContain("<script>");
    expect(document.querySelector("[data-testid=markdown] script")).toBeNull();
    expect(document.querySelector("[data-testid=markdown] img")).toBeNull();

    // Default: the newest release and its default variant.
    await waitFor(() => {
      expect(
        document.querySelector('[data-forge-cli="forge tool install php@8.4.27 --variant nts"]'),
      ).not.toBeNull();
    });
    // The plan total is shown before the install.
    await waitFor(() => {
      expect(screen.getByTestId("plan-total")).toHaveTextContent("Total download: 31 MB");
    });

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Choose version 8.3.40" }));
    await waitFor(() => {
      expect(
        document.querySelector('[data-forge-cli="forge tool install php@8.3.40 --variant nts"]'),
      ).not.toBeNull();
    });
    await user.click(screen.getByRole("button", { name: "Choose version 8.4.26" }));

    await user.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() => {
      expect(document.querySelector("[data-install-job]")).not.toBeNull();
    });
    const install = h.fake.calls.find((call) => call.method === "tools.install");
    expect(install?.params).toEqual({ specs: ["php@8.4.26"], variant: "nts", yes: true });
    await waitFor(() => {
      expect(document.querySelector('[data-install-job][data-state="done"]')).not.toBeNull();
    });
    expect(document.querySelector('[data-part="check"]')).not.toBeNull();
  });

  it("shows a banner with the cached list when the providers are unreachable (stale)", async () => {
    await open("/catalog/nodejs");
    const banner = await screen.findByTestId("stale-banner");
    expect(banner).toHaveTextContent(/could not be reached/);
    expect(banner).toHaveTextContent(/cached on/);
    // The cached data is still listed.
    expect(screen.getByText("Series 22.11 (1 version)")).toBeInTheDocument();
  });

  it("shows the cause and a retry when the releases fail", async () => {
    const h = await open("/catalog/legacy");
    await waitFor(() => {
      expect(document.querySelector("[data-error-notice]")).not.toBeNull();
    });
    const alert = within(document.body)
      .getAllByRole("alert")
      .find((el) => el.hasAttribute("data-error-notice"));
    if (alert === undefined) throw new Error("no error notice");
    expect(alert).toHaveAttribute("data-error-code", "network");
    const calls = () => h.fake.calls.filter((call) => call.method === "tools.releases").length;
    expect(calls()).toBe(1);
    await userEvent.setup().click(within(alert).getByRole("button", { name: /try again|retry/i }));
    await waitFor(() => {
      expect(calls()).toBe(2);
    });
  });

  it("says why versions are unavailable while the daemon does not offer tools.releases", async () => {
    const h = await open("/catalog/php", {
      extra: { notImplemented: ["tools.releases", "tools.plan"] },
    });
    await screen.findByRole("heading", { level: 1, name: "PHP" });
    expect(
      await screen.findByText(/not available yet|not implemented|M2-E2-T5/i),
    ).toBeInTheDocument();
    expect(h.fake.calls.filter((call) => call.method === "tools.releases")).toHaveLength(0);
    expect(screen.queryByRole("button", { name: "Install" })).toBeNull();
  });

  it("cancels an install in progress", async () => {
    const h = await open("/catalog/redis");
    await screen.findByRole("heading", { level: 1, name: "Redis" });
    const user = userEvent.setup();
    const install = await screen.findByRole("button", { name: "Install" });
    await waitFor(() => {
      expect(install).toBeEnabled();
    });
    await user.click(install);
    await user.click(await screen.findByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(document.querySelector('[data-install-job][data-state="cancelled"]')).not.toBeNull();
    });
    expect(h.fake.calls.filter((call) => call.method === "jobs.cancel")).toHaveLength(1);
  });
});

describe("Stack page", () => {
  it("shows the plan total before confirming and installs the planned specs", async () => {
    const h = await open("/catalog/laravel");
    await screen.findByRole("heading", { level: 1, name: "Laravel" });
    const total = await screen.findByTestId("plan-total");
    // 31 + 190 (estimated) + 12 MB.
    expect(total).toHaveTextContent("Total download: ≈ 233 MB");
    expect(h.fake.calls.some((call) => call.method === "tools.install")).toBe(false);
    expect(screen.getByRole("button", { name: /Create project/ })).toBeDisabled();

    const confirm = screen.getByRole("button", { name: "Confirm and install (≈ 233 MB)" });
    await waitFor(() => {
      expect(confirm).toBeEnabled();
    });
    expect(
      document.querySelector(
        '[data-forge-cli="forge tool install php@8.4.27 mysql@8.4.3 redis@7.4.1"]',
      ),
    ).not.toBeNull();
    await userEvent.setup().click(confirm);
    await waitFor(() => {
      expect(h.fake.calls.find((call) => call.method === "tools.install")?.params).toEqual({
        specs: ["php@8.4.27", "mysql@8.4.3", "redis@7.4.1"],
        yes: true,
      });
    });
    expect(await screen.findByRole("progressbar")).toBeInTheDocument();
  });
});
