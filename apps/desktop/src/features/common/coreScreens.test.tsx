// M2-E4-T9: regression tests of the core-screen findings of docs/design/qa/M2-E4-report.md.
// R-09/R-10 (Sites and the site detail sat on a skeleton for ever when `sites.list` failed),
// R-16 (the same for a daemon that answers `not_implemented`), R-12 (the eight actions the
// parity gate listed as pending) and R-18 (the holder of a port).
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { catalogHandlers } from "../../../qa/fixtures/scenarios/catalog";
import { daemonStatus } from "../../../qa/fixtures/scenarios/data";
import { phpHandlers } from "../../../qa/fixtures/scenarios/php";
import { siteDetailHandlers } from "../../../qa/fixtures/scenarios/siteDetail";
import { renderApp, type AppHarness } from "../../app/testApp";
import { fakeError, type FakeHandlers } from "../../ipc/fakeBridge";
import { populatedData, tool, type ScenarioData } from "../../state/testFixtures";

vi.mock("../../ipc/native", () => ({
  copyText: () => Promise.resolve(true),
  openUrl: () => Promise.resolve(true),
  pickPath: () => Promise.resolve(null),
}));

let harness: AppHarness | null = null;
let failing = true;

beforeEach(() => {
  failing = true;
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get: () => 1000,
  });
});

afterEach(async () => {
  delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
  await harness?.layer.dispose();
  harness = null;
});

const SITE_ID = "01JSITE00000000000000000LJA";

/** A `sites.list` that fails until the test flips `failing` (the retry case). */
const failingSites: FakeHandlers = {
  "sites.list": () => {
    if (failing) fakeError("internal", "sites.list failed", "fake_error", { method: "sites.list" });
    return { sites: [] };
  },
};

async function open(
  path: string,
  extra: { notImplemented?: string[]; handlers?: FakeHandlers } = {},
  data: ScenarioData = populatedData(),
) {
  harness = await renderApp({
    path,
    data,
    extra: {
      notImplemented: extra.notImplemented ?? [],
      handlers: { ...extra.handlers },
    },
  });
  return harness;
}

describe("Sites: a failed or unavailable list explains itself (R-09, R-10, R-16)", () => {
  it("shows the cause and a working retry when sites.list fails", async () => {
    await open("/sites", { handlers: failingSites });
    const alert = await screen.findByTestId("no-data-error");
    expect(within(alert).getByRole("alert")).toHaveAttribute("data-error-code", "internal");
    expect(document.querySelector('[aria-busy="true"]')).toBeNull();
    failing = false;
    await userEvent.setup().click(within(alert).getByRole("button", { name: "Try again" }));
    await waitFor(() => {
      expect(screen.queryByTestId("no-data-error")).toBeNull();
    });
    expect(await screen.findByText("No sites yet")).toBeInTheDocument();
  });

  it("says not implemented instead of an endless skeleton", async () => {
    await open("/sites", { notImplemented: ["sites.list"] });
    const alert = await screen.findByTestId("no-data-error");
    expect(within(alert).getByRole("alert")).toHaveAttribute("data-error-code", "not_implemented");
    expect(alert).toHaveTextContent("Not available yet in this version of Forgebox.");
  });

  it("does the same on the site detail", async () => {
    await open(`/sites/${SITE_ID}`, { handlers: failingSites });
    const alert = await screen.findByTestId("no-data-error");
    expect(within(alert).getByRole("alert")).toHaveAttribute("data-error-code", "internal");
    expect(screen.getByRole("heading", { level: 1, name: "Sites" })).toBeInTheDocument();
  });
});

describe("Other core screens say not implemented (R-16)", () => {
  it("Services", async () => {
    await open("/services", { notImplemented: ["services.status"] });
    const view = await waitFor(() => {
      const el = document.querySelector('[data-async-view="error"]');
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    expect(within(view).getByRole("alert")).toHaveAttribute("data-error-code", "not_implemented");
  });

  it("Dashboard tiles", async () => {
    await open("/dashboard", { notImplemented: ["services.status"] });
    const tiles = await screen.findByTestId("dashboard-tiles");
    await waitFor(() => {
      expect(within(tiles).getByRole("alert")).toHaveAttribute(
        "data-error-code",
        "not_implemented",
      );
    });
  });

  it("PHP", async () => {
    await open("/php", { notImplemented: ["php.list"] });
    const page = await screen.findByTestId("page-php");
    await waitFor(() => {
      expect(within(page).getByRole("alert")).toHaveAttribute("data-error-code", "not_implemented");
    });
    expect(page.querySelector('[aria-busy="true"]')).toBeNull();
  });

  it("the tool page", async () => {
    await open("/catalog/php", { notImplemented: ["tools.info"] });
    await waitFor(() => {
      expect(document.querySelector("[data-error-notice]")).toHaveAttribute(
        "data-error-code",
        "not_implemented",
      );
    });
    expect(screen.queryByTestId("tool-loading")).toBeNull();
  });
});

describe("The eight actions the parity gate listed as pending (R-12)", () => {
  function phpData(): ScenarioData {
    const data = populatedData();
    data.tools = [
      ...data.tools,
      tool("php", { name: "PHP", category: "runtime", current: "8.3.36" }),
    ];
    data.pools = [];
    return data;
  }

  async function openPhp() {
    const data = phpData();
    const qa = {
      instances: [],
      sites: data.sites.map((s) => ({ ...s })),
      tools: data.tools,
      daemon: daemonStatus(),
    };
    harness = await renderApp({
      path: "/php",
      data,
      extra: { notImplemented: [], handlers: phpHandlers(qa, {}) },
    });
    return await screen.findByTestId("php-columns");
  }

  const callsOf = (method: string) =>
    harness?.fake.calls.filter((call) => call.method === method) ?? [];

  it("PHP column: reload the pool, open php.ini, apply the found update", async () => {
    const user = userEvent.setup();
    const columns = await openPhp();
    const column = within(columns.querySelector<HTMLElement>('[data-php-series="8.4"]') ?? columns);
    await user.click(await column.findByRole("button", { name: "Open php.ini" }));
    await waitFor(() => {
      expect(callsOf("php.ini_open")[0]?.params).toEqual({ version: "8.4" });
    });
    const c83 = within(columns.querySelector<HTMLElement>('[data-php-series="8.3"]') ?? columns);
    await user.click(await c83.findByRole("button", { name: "Reload pool" }));
    await waitFor(() => {
      expect(callsOf("services.reload")[0]?.params).toEqual({ target: "php@8.3" });
    });
    const updates = within(await c83.findByTestId("updates"));
    expect(updates.queryByRole("button", { name: "Apply update" })).toBeNull();
    await user.click(updates.getByRole("button", { name: "Check now" }));
    await user.click(await updates.findByRole("button", { name: "Apply update" }));
    await waitFor(() => {
      expect(callsOf("tools.update")[0]?.params).toEqual({ tool: "php" });
    });
  });

  it("Catalog: scan bin/ folders", async () => {
    const data = populatedData();
    harness = await renderApp({
      path: "/catalog",
      data,
      extra: {
        notImplemented: [],
        handlers: catalogHandlers({ instances: [], sites: [], tools: [], daemon: data.daemon }),
      },
    });
    await userEvent.setup().click(await screen.findByRole("button", { name: "Scan bin/ folders" }));
    await waitFor(() => {
      expect(callsOf("tools.discover")).toHaveLength(1);
    });
  });

  it("Site detail: HTTPS, web server and the project up/down", async () => {
    const user = userEvent.setup();
    const data = populatedData();
    await open(`/sites/${SITE_ID}`, { handlers: siteDetailHandlers(data) }, data);
    const general = await screen.findByTestId("site-general");
    await user.click(within(general).getByRole("button", { name: "Turn off HTTPS" }));
    await waitFor(() => {
      expect(callsOf("sites.secure")[0]?.params).toEqual({ site: SITE_ID, secure: false });
    });
    // The server button stays off until another server is chosen.
    expect(within(general).getByRole("button", { name: "Switch server" })).toBeDisabled();
    await user.click(await screen.findByRole("tab", { name: "Processes" }));
    const project = await screen.findByTestId("project-actions");
    await user.click(within(project).getByRole("button", { name: "Start project" }));
    await waitFor(() => {
      expect(callsOf("sites.up")[0]?.params).toEqual({ dir: "C:/Forgebox/www/loja.test" });
    });
    await user.click(within(project).getByRole("button", { name: "Stop project" }));
    await waitFor(() => {
      expect(callsOf("sites.down")[0]?.params).toEqual({ dir: "C:/Forgebox/www/loja.test" });
    });
  });
});
