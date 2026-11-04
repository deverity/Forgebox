// Acceptance of M2-E2-T17 (docs/07 section 5.5): three series in three columns, the CLI radio
// that follows tool.current_changed, optimistic extension toggles with rollback, Xdebug with
// install progress, and the pool req/s with tabular figures.
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { phpHandlers } from "../../../qa/fixtures/scenarios/php";
import { daemonStatus } from "../../../qa/fixtures/scenarios/data";
import { renderApp } from "../../app/testApp";
import { ingestMetrics } from "../../state/metrics";
import { populatedData, tool, type ScenarioData } from "../../state/testFixtures";

function phpData(): ScenarioData {
  const data = populatedData();
  data.tools = [
    ...data.tools,
    tool("php", { name: "PHP", category: "runtime", current: "8.3.36" }),
  ];
  data.pools = [];
  return data;
}

/** `holdJobs`: methods whose jobs run until the test ends them (`FakeBridge.completeJob`). */
async function mountPhp(data: ScenarioData = phpData(), holdJobs: readonly string[] = []) {
  const qa = {
    instances: [],
    sites: data.sites.map((s) => ({ ...s })),
    tools: data.tools,
    daemon: daemonStatus(),
  };
  const h = await renderApp({
    path: "/php",
    data,
    extra: { notImplemented: [], handlers: phpHandlers(qa, {}), holdJobs },
  });
  await screen.findByTestId("php-columns");
  return h;
}

function column(series: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-php-series="${series}"]`);
  if (el === null) throw new Error(`no column ${series}`);
  return el;
}

describe("series columns and the CLI radio", () => {
  it("shows one column per installed series, oldest first, with version and variant", async () => {
    await mountPhp();
    const columns = [...document.querySelectorAll<HTMLElement>("[data-php-series]")];
    expect(columns.map((c) => c.dataset.phpSeries)).toEqual(["8.2", "8.3", "8.4"]);
    const c84 = within(column("8.4"));
    expect(c84.getByTestId("exact-version")).toHaveTextContent("8.4.1");
    expect(c84.getByText("NTS")).toBeInTheDocument();
    expect(c84.getByText("vs17")).toBeInTheDocument();
    expect(c84.getByTestId("series-sites")).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/sites\?php=/),
    );
    expect(within(column("8.3")).getByTestId("series-sites")).toHaveTextContent("1 site");
    // The region scrolls sideways instead of squeezing the columns.
    expect(screen.getByTestId("php-columns").className).toContain("overflow-x-auto");
    expect(column("8.4").className).toContain("shrink-0");
  });

  it("calls tools.use and moves the radio when tool.current_changed arrives", async () => {
    const user = userEvent.setup();
    const h = await mountPhp();
    const radio = (series: string) =>
      within(column(series)).getByRole("radio", { name: /Default CLI/i });
    expect(radio("8.3")).toBeChecked();
    expect(radio("8.4")).not.toBeChecked();
    const label = radio("8.4").closest("label");
    expect(label).toHaveAttribute("data-forge-cli", "forge use php 8.4");

    await user.click(radio("8.4"));
    await waitFor(() => {
      expect(h.fake.calls.filter((c) => c.method === "tools.use")).toHaveLength(1);
    });
    expect(h.fake.calls.find((c) => c.method === "tools.use")?.params).toEqual({
      tool: "php",
      version: "8.4",
    });
    await waitFor(() => {
      expect(radio("8.4")).toBeChecked();
    });
    expect(radio("8.3")).not.toBeChecked();
  });
});

describe("extensions", () => {
  it("searches the list and marks bundled, registry and zend extensions", async () => {
    const user = userEvent.setup();
    await mountPhp();
    const ext = within(await within(column("8.3")).findByTestId("extensions"));
    await ext.findByText("intl");
    expect(ext.getByText("60 of 60")).toBeInTheDocument();
    const opcache = ext.getByText("opcache").closest("li");
    expect(within(opcache as HTMLElement).getByTestId("zend-marker")).toBeInTheDocument();
    expect(within(opcache as HTMLElement).getByText("Bundled")).toBeInTheDocument();
    expect(
      within(ext.getByText("redis").closest("li") as HTMLElement).getByText("Registry"),
    ).toBeInTheDocument();

    await user.type(ext.getByRole("searchbox", { name: "Search extensions" }), "pdo");
    expect(ext.getByText("pdo_mysql")).toBeInTheDocument();
    expect(ext.queryByText("intl")).not.toBeInTheDocument();
    await user.clear(ext.getByRole("searchbox"));
    await user.type(ext.getByRole("searchbox"), "zzz");
    expect(ext.getByText("No extension matches “zzz”.")).toBeInTheDocument();
  });

  it("is optimistic and sends the CLI-equivalent job", async () => {
    const user = userEvent.setup();
    const h = await mountPhp();
    const ext = within(await within(column("8.4")).findByTestId("extensions"));
    const toggle = await ext.findByRole("switch", { name: "intl extension" });
    expect(toggle).toBeChecked();
    expect(toggle.closest("li")).toHaveAttribute(
      "data-forge-cli",
      "forge php ext disable 8.4 intl",
    );
    await user.click(toggle);
    // Same frame: the new state is shown before the job ends.
    expect(ext.getByRole("switch", { name: "intl extension" })).not.toBeChecked();
    expect(ext.getByRole("switch", { name: "intl extension" }).closest("li")).toHaveAttribute(
      "data-pending",
      "true",
    );
    await waitFor(() => {
      expect(h.fake.calls.find((c) => c.method === "php.ext_disable")?.params).toEqual({
        version: "8.4",
        extension: "intl",
      });
    });
    await waitFor(() => {
      expect(ext.getByRole("switch", { name: "intl extension" }).closest("li")).not.toHaveAttribute(
        "data-pending",
      );
    });
    expect(ext.getByRole("switch", { name: "intl extension" })).not.toBeChecked();
  });

  it("rolls the toggle back when the job fails and says why", async () => {
    const user = userEvent.setup();
    await mountPhp();
    const ext = within(await within(column("8.3")).findByTestId("extensions"));
    const toggle = await ext.findByRole("switch", { name: "sodium extension" });
    expect(toggle).not.toBeChecked();
    await user.click(toggle);
    expect(ext.getByRole("switch", { name: "sodium extension" })).toBeChecked();
    await waitFor(() => {
      expect(
        ext.getAllByRole("alert").some((a) => a.textContent.includes("Could not change sodium")),
      ).toBe(true);
    });
    expect(ext.getByRole("switch", { name: "sodium extension" })).not.toBeChecked();
  });
});

describe("Xdebug", () => {
  it("installs a missing extension with progress, then shows trigger mode", async () => {
    const user = userEvent.setup();
    // The install runs until the test ends it: its step timers coalesce on a loaded machine
    // (full vitest run) and the row could come and go between two assertions.
    const h = await mountPhp(phpData(), ["php.xdebug"]);
    const xdebug = within(await within(column("8.4")).findByTestId("xdebug"));
    expect(xdebug.getByRole("radio", { name: "Off" })).toBeChecked();
    await user.click(xdebug.getByRole("radio", { name: "Trigger" }));

    // The mode already reads trigger and the install job reports its progress.
    const row = await waitFor(() => {
      const found = column("8.4").querySelector<HTMLElement>("[data-install-job]");
      if (found === null) throw new Error("no install row in the 8.4 column");
      return found;
    });
    expect(within(row).getByText("Installing Xdebug for PHP 8.4")).toBeInTheDocument();
    expect(within(column("8.4")).getByTestId("xdebug")).toHaveAttribute("data-mode", "trigger");
    // A step of the install reached the row (which one depends on how fast the timers ran).
    await waitFor(
      () => {
        expect(row.querySelector('[data-part="step"]')?.textContent).toMatch(
          /downloading|installing/,
        );
      },
      { timeout: 5000 },
    );
    expect(h.fake.calls.find((c) => c.method === "php.xdebug")?.params).toEqual({
      version: "8.4",
      mode: "trigger",
    });
    const job = h.fake.jobList().find((j) => j.name === "php.xdebug" && j.state === "running");
    if (job === undefined) throw new Error("no running php.xdebug job");
    act(() => {
      expect(h.fake.completeJob(job.id)).toBe(true);
      h.fake.flush();
    });
    await waitFor(() => {
      expect(document.querySelector("[data-install-job]")).toBeNull();
      expect(within(column("8.4")).getByTestId("xdebug")).toHaveAttribute("data-mode", "trigger");
    });
    expect(within(column("8.4")).getByRole("radio", { name: "Trigger" })).toBeChecked();
    expect(
      column("8.4").querySelector('[data-forge-cli="forge php xdebug 8.4 trigger"]'),
    ).not.toBeNull();
  });

  it("refuses a port that is not a port before sending anything", async () => {
    const user = userEvent.setup();
    const h = await mountPhp();
    const xdebug = within(await within(column("8.3")).findByTestId("xdebug"));
    await user.click(xdebug.getByRole("radio", { name: "On" }));
    await waitFor(() => {
      expect(within(column("8.3")).getByTestId("xdebug")).toHaveAttribute("data-mode", "on");
    });
    const port = xdebug.getByRole("textbox", { name: "Client port" });
    await user.type(port, "99999");
    fireEvent.blur(port);
    expect(xdebug.getAllByText("Use a port between 1 and 65535.").length).toBeGreaterThan(0);
    const apply = column("8.3").querySelector('[data-action="php.xdebug"]');
    expect(apply).toHaveAttribute("data-disabled-reason", "Use a port between 1 and 65535.");
    expect(h.fake.calls.filter((c) => c.method === "php.xdebug")).toHaveLength(1);
  });
});

describe("pool card", () => {
  it("shows req/s from the requests_total delta (100 to 140 in 2 s is 20.0) with tabular figures", async () => {
    const h = await mountPhp();
    const card = within(column("8.3")).getByTestId("pool-card");
    expect(within(card).getByTestId("pool-rate")).toHaveTextContent("— req/s");
    expect(within(card).getByTestId("pool-workers")).toHaveTextContent("1/4 workers");

    const runtime = h.layer.runtime("local");
    const listed = await runtime.api.call("php.list", {});
    const pool = listed.versions.find((v) => v.series === "8.3")?.pool;
    if (pool === null || pool === undefined) throw new Error("no pool");
    const first = ingestMetrics(
      runtime.stores.metrics.getState(),
      { instances: [], pools: [{ ...pool, requests_total: 100 }] },
      null,
      0,
    );
    const second = ingestMetrics(
      first,
      { instances: [], pools: [{ ...pool, requests_total: 140 }] },
      null,
      2000,
    );
    act(() => {
      runtime.stores.metrics.setState(second);
    });
    const rate = await within(column("8.3")).findByTestId("pool-rate");
    await waitFor(() => {
      expect(rate).toHaveTextContent("20.0 req/s");
    });
    expect(rate.parentElement?.className).toContain("tabular-nums");
  });
});

describe("updates", () => {
  it("explains the per-tool policy, checks now and offers the rollback of the last update", async () => {
    const user = userEvent.setup();
    const h = await mountPhp();
    const updates = within(await within(column("8.3")).findByTestId("updates"));
    expect(updates.getByText(/The policy applies to all PHP versions/)).toBeInTheDocument();
    expect(await updates.findByText("8.3.35 → 8.3.36 · Updated")).toBeInTheDocument();
    await user.click(updates.getByRole("button", { name: "Check now" }));
    expect(await updates.findByText("PHP 8.3.36 can update to 8.3.37.")).toBeInTheDocument();
    await user.click(updates.getByRole("button", { name: "Roll back" }));
    await waitFor(() => {
      expect(h.fake.calls.find((c) => c.method === "tools.rollback")?.params).toEqual({
        spec: "php@8.3",
      });
    });
    // The other series have no history to undo.
    expect(
      within(column("8.2")).queryByRole("button", { name: "Roll back" }),
    ).not.toBeInTheDocument();
  });
});

describe("empty state", () => {
  it("offers to install PHP 8.4, which opens the catalog on php", async () => {
    const user = userEvent.setup();
    const data = populatedData();
    const qa = { instances: [], sites: [], tools: [], daemon: daemonStatus() };
    const h = await renderApp({
      path: "/php",
      data,
      extra: { notImplemented: [], handlers: phpHandlers(qa, {}) },
    });
    expect(await screen.findByText("No PHP installed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Install PHP 8.4" }));
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/catalog/php");
    });
  });

  it("the Install another version button opens /catalog/php", async () => {
    const user = userEvent.setup();
    const h = await mountPhp();
    await user.click(screen.getByRole("button", { name: "Install another version" }));
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/catalog/php");
    });
  });
});
