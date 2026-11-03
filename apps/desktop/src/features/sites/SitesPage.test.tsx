import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { heavyData, site as qaSite, type QaData } from "../../../qa/fixtures/scenarios/data";
import { siteHandlers } from "../../../qa/fixtures/scenarios";
import { renderApp, type AppHarness } from "../../app/testApp";
import { NEW_SITE_EVENT } from "../../app/palette/actions";
import { populatedData, type ScenarioData } from "../../state/testFixtures";

let harness: AppHarness | null = null;
// jsdom has no layout: give every element the viewport the virtualizer and the column count read.
beforeEach(() => {
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
  window.localStorage.clear();
});

function scenarioData(sites: QaData["sites"]): ScenarioData {
  const data = populatedData();
  data.sites = sites;
  return data;
}

async function open(
  data: ScenarioData,
  extra: Parameters<typeof renderApp>[0] = {},
): Promise<AppHarness> {
  harness = await renderApp({
    path: "/sites",
    data,
    ...extra,
    extra: { handlers: siteHandlers(data), ...extra.extra },
  });
  await screen.findByRole("heading", { level: 1, name: "Sites" });
  await waitFor(() => {
    expect(document.querySelector("[data-site-card]")).not.toBeNull();
  });
  return harness;
}

const cards = () => document.querySelectorAll("[data-site-card]");

async function openSheet() {
  await userEvent.setup().click(screen.getByRole("button", { name: /New site/ }));
  return screen.findByRole("dialog");
}

describe("Sites screen", () => {
  it("filters 500 sites in under 50 ms per keystroke with fewer than 60 nodes mounted", async () => {
    await open(scenarioData(heavyData(500, 0).sites));
    expect(cards().length).toBeGreaterThan(0);
    expect(cards().length).toBeLessThan(60);
    const search = screen.getByRole("searchbox", { name: "Search sites" });

    const timings: number[] = [];
    for (const query of ["s", "si", "site4", "site42"]) {
      const start = performance.now();
      fireEvent.change(search, { target: { value: query } });
      timings.push(performance.now() - start);
      expect(cards().length).toBeLessThan(60);
    }
    // jsdom renders several times slower than a browser: this is a regression guard (the real
    // < 50 ms measurement is e2e/sites.spec.ts). The first keystroke warms the code paths.
    expect(Math.max(...timings.slice(1)), timings.join(", ")).toBeLessThan(150);
    expect(cards().length).toBeGreaterThan(0);
    expect(screen.getAllByText("site42.test").length).toBeGreaterThan(0);

    fireEvent.change(search, { target: { value: "zzz-no-match" } });
    expect(cards()).toHaveLength(0);
    expect(screen.getByText(/No site matches/)).toBeInTheDocument();
  });

  it("persists the layout and renders rows in list mode", async () => {
    await open(scenarioData([qaSite(1, "loja.test"), qaSite(2, "blog.test")]));
    expect(cards()[0]?.getAttribute("data-layout")).toBe("grid");
    await userEvent.setup().click(screen.getByRole("radio", { name: "List" }));
    await waitFor(() => {
      expect(cards()[0]?.getAttribute("data-layout")).toBe("list");
    });
    expect(window.localStorage.getItem("forgebox.sites.layout")).toBe("list");
  });

  it("shows driver, badges and the web process state on the card", async () => {
    const web = {
      site_id: "01QAS1TE00000000000000000A",
      host: "api.test",
      name: "web",
      command: "npm run dev",
      state: "running" as const,
      pid: 1,
      port: 3000,
      autostart: false,
    };
    await open(
      scenarioData([
        qaSite(1, "loja.test", { driver: "laravel" }),
        qaSite(2, "api.test", { processes: [web] }),
      ]),
    );
    const card =
      document
        .querySelector('[data-site-card] [aria-label="Open loja.test in the browser"]')
        ?.closest<HTMLElement>("[data-site-card]") ?? document.body;
    expect(within(card).getByText("PHP 8.3.36")).toBeInTheDocument();
    expect(within(card).getByText("HTTPS")).toBeInTheDocument();
    expect(within(card).getByText("laravel")).toBeInTheDocument();
    expect(within(card).getByText("Aa")).toBeInTheDocument();
    expect(within(card).getByRole("img", { name: "Driver laravel" })).toHaveTextContent("LA");
    expect(screen.getByText("web: running")).toBeInTheDocument();
  });

  it("lowercases a typed host with a note", async () => {
    await open(scenarioData([qaSite(1, "loja.test")]));
    const dialog = await openSheet();
    const host = within(dialog).getByLabelText("Host");
    fireEvent.change(host, { target: { value: "Loja.Test" } });
    expect(host).toHaveValue("loja.test");
    expect(
      within(dialog).getByText(/Hosts are always lowercase: using "loja.test"/),
    ).toBeInTheDocument();
  });

  it("suggests <folder>.<tld> from the folder", async () => {
    await open(scenarioData([qaSite(1, "loja.test")]));
    const dialog = await openSheet();
    fireEvent.change(within(dialog).getByLabelText("Project folder"), {
      target: { value: "C:\\Meus Projetos\\Minha Loja" },
    });
    expect(within(dialog).getByLabelText("Host")).toHaveValue("minha-loja.test");
    expect(within(dialog).getByText(/^forge site add /)).toBeInTheDocument();
  });

  it("maps a bad-TLD invalid_input to an inline error on the host field", async () => {
    const h = await open(scenarioData([qaSite(1, "loja.test")]));
    const dialog = await openSheet();
    fireEvent.change(within(dialog).getByLabelText("Project folder"), {
      target: { value: "C:\\www\\loja2" },
    });
    fireEvent.change(within(dialog).getByLabelText("Host"), { target: { value: "loja2.invalid" } });
    await userEvent.setup().click(within(dialog).getByRole("button", { name: "Create site" }));
    const hostField = within(dialog).getByLabelText("Host");
    await waitFor(() => {
      expect(hostField).toHaveAttribute("aria-invalid", "true");
    });
    expect(h.fake.calls.some((call) => call.method === "sites.add")).toBe(true);
    expect(within(dialog).getAllByRole("alert").length).toBeGreaterThan(0);
  });

  it("creates a site from a folder and from a proxy", async () => {
    const h = await open(scenarioData([qaSite(1, "loja.test")]));
    const user = userEvent.setup();
    let dialog = await openSheet();
    fireEvent.change(within(dialog).getByLabelText("Project folder"), {
      target: { value: "C:\\www\\nova" },
    });
    await user.click(within(dialog).getByRole("button", { name: "Create site" }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    const add = h.fake.calls.find((call) => call.method === "sites.add");
    expect(add?.params).toMatchObject({ dir: "C:\\www\\nova", host: "nova.test", secure: true });
    expect(await screen.findByText("nova.test")).toBeInTheDocument();

    dialog = await openSheet();
    await user.click(within(dialog).getByRole("tab", { name: "Proxy" }));
    fireEvent.change(within(dialog).getByLabelText("Host"), { target: { value: "app.test" } });
    fireEvent.change(within(dialog).getByLabelText("Upstream"), {
      target: { value: "http://127.0.0.1:3000" },
    });
    await user.click(within(dialog).getByRole("switch", { name: /WebSocket/ }));
    expect(
      within(dialog).getByText(/^forge proxy add app.test http:\/\/127.0.0.1:3000 --ws/),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Create site" }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    const proxy = h.fake.calls.find((call) => call.method === "sites.proxy_add");
    expect(proxy?.params).toMatchObject({
      host: "app.test",
      upstream: "http://127.0.0.1:3000",
      websocket: true,
    });
  });

  it("keeps 'New project' disabled", async () => {
    await open(scenarioData([qaSite(1, "loja.test")]));
    const dialog = await openSheet();
    expect(within(dialog).getByRole("tab", { name: "New project" })).toBeDisabled();
  });

  it("disables drag and drop and Browse with a reason on a remote daemon", async () => {
    await open(scenarioData([qaSite(1, "loja.test")]), { extra: { hello: { local: false } } });
    const dialog = await openSheet();
    const zone = within(dialog).getByTestId("drop-zone");
    expect(zone).toHaveAttribute("aria-disabled", "true");
    expect(zone).toHaveAttribute("data-drop-state", "disabled");
    expect(zone.textContent).toMatch(/another machine/);
    const browse = within(dialog).getByRole("button", { name: "Browse…" });
    expect(browse).toBeDisabled();
    expect(browse.getAttribute("data-disabled-reason")).toMatch(/another machine/);
  });

  it("inserts a card on site.created without a list query of its own", async () => {
    const data = scenarioData([qaSite(1, "loja.test")]);
    const h = await open(data);
    const before = h.fake.calls.filter((call) => call.method === "sites.list").length;
    const created = qaSite(2, "parked.test");
    data.sites.push(created);
    await act(async () => {
      h.fake.publish("site.created", { v: 1, site_id: created.id, host: created.host });
      await h.layer.runtime("local").settled();
    });
    expect(await screen.findByText("parked.test")).toBeInTheDocument();
    // The state layer coalesces the event into one snapshot; the screen asks for nothing.
    const after = h.fake.calls.filter((call) => call.method === "sites.list").length;
    expect(after - before).toBeLessThanOrEqual(1);
    expect(h.fake.calls.some((call) => call.method === "sites.info")).toBe(false);
  });

  it("removes with confirm: true after saying the files are kept", async () => {
    const h = await open(scenarioData([qaSite(1, "loja.test"), qaSite(2, "blog.test")]));
    await userEvent.setup().click(screen.getByRole("button", { name: "Remove blog.test" }));
    const dialog = await screen.findByTestId("remove-site-dialog");
    expect(within(dialog).getByText(/project files are never deleted/)).toBeInTheDocument();
    await userEvent.setup().click(within(dialog).getByRole("button", { name: "Remove site" }));
    await waitFor(() => {
      expect(screen.queryByTestId("remove-site-dialog")).toBeNull();
    });
    const call = h.fake.calls.find((c) => c.method === "sites.remove");
    expect(call?.params).toMatchObject({ confirm: true });
    await waitFor(() => {
      expect(screen.queryByText("blog.test")).toBeNull();
    });
    expect(screen.getByText("loja.test")).toBeInTheDocument();
  });

  it("opens the create sheet on the palette's new-site request", async () => {
    await open(scenarioData([qaSite(1, "loja.test")]));
    act(() => {
      window.dispatchEvent(new CustomEvent(NEW_SITE_EVENT));
    });
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
  });

  it("shows the empty state with a create action", async () => {
    harness = await renderApp({ path: "/sites", data: scenarioData([]) });
    expect(await screen.findByText("No sites yet")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /New site/ }).length).toBeGreaterThan(0);
  });
});
