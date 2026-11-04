import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "i18next";
import { Suspense } from "react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

// jsdom has no canvas: uPlot is replaced by a recorder with the surface the charts use.
const instances = vi.hoisted(() => [] as FakeUPlot[]);

interface FakeUPlot {
  opts: Record<string, unknown>;
  data: unknown;
  setData: ReturnType<typeof vi.fn>;
  setSize: ReturnType<typeof vi.fn>;
  destroy: ReturnType<typeof vi.fn>;
}

vi.mock("uplot", () => {
  class Fake {
    over = document.createElement("div");
    scales: Record<string, { min: number; max: number }>;
    width: number;
    height: number;
    setData = vi.fn();
    setSize = vi.fn();
    destroy = vi.fn();
    constructor(
      public opts: {
        width: number;
        height: number;
        hooks?: { draw?: ((u: Fake) => void)[] };
      },
      public data: number[][],
    ) {
      const xs = data[0] ?? [];
      this.scales = { x: { min: xs[0] ?? 0, max: xs.at(-1) ?? 1 } };
      this.width = opts.width;
      this.height = opts.height;
      Object.defineProperty(this.over, "offsetHeight", { value: 150 });
      instances.push(this);
      for (const hook of opts.hooks?.draw ?? []) hook(this);
    }
    valToPos(value: number): number {
      const { min, max } = this.scales.x as { min: number; max: number };
      return ((value - min) / (max - min)) * 500;
    }
  }
  return { default: Fake };
});

import ChartSparkline from "./ChartSparkline";
import LatencyBandChart from "./LatencyBandChart";
import ResourceChart from "./ResourceChart";
import ThroughputErrorsChart from "./ThroughputErrorsChart";
import type { ChartMarker, LatencySeries } from "./types";

const T = [1000, 1002, 1004, 1006];
const latency: LatencySeries = {
  t: T,
  p50: [10, 11, 12, 13],
  p90: [20, 22, 24, 26],
  p99: [40, 44, 48, 52],
};
const markers: ChartMarker[] = [
  { id: "a", t: 1002, kind: "restart" },
  { id: "b", t: 1004, kind: "php-version", detail: "8.3 to 8.4" },
  { id: "far", t: 9999, kind: "update" },
];

beforeAll(async () => {
  await i18n.loadNamespaces("metrics");
  // jsdom lays nothing out: give the charts a width.
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, value: 600 });
});

beforeEach(() => {
  instances.length = 0;
});

afterEach(() => {
  document.documentElement.removeAttribute("style");
  delete document.documentElement.dataset.theme;
});

const wrap = (ui: React.ReactNode) => <Suspense fallback={null}>{ui}</Suspense>;

describe("chart states", () => {
  it("shows a fixed-height skeleton while loading", () => {
    render(<LatencyBandChart series={null} loading height={180} />);
    const slot = document.querySelector<HTMLElement>("[data-skeleton-slot]");
    expect(slot?.style.height).toBe("180px");
    expect(slot).toHaveAttribute("aria-busy", "true");
    expect(screen.getByText("Loading chart")).toBeInTheDocument();
    expect(instances).toHaveLength(0);
  });

  it("shows the empty state with the same slot height", () => {
    render(<LatencyBandChart series={null} height={180} />);
    expect(screen.getByText("No data yet")).toBeInTheDocument();
    expect(document.querySelector<HTMLElement>("[data-skeleton-slot]")?.style.height).toBe("180px");
    expect(instances).toHaveLength(0);
  });

  it("mounts uPlot with data and lists every percentile in the aria summary", () => {
    render(wrap(<LatencyBandChart series={latency} />));
    expect(instances).toHaveLength(1);
    const table = screen.getByRole("table", { name: "Latency: summary" });
    for (const label of ["p50", "p90", "p99"]) {
      expect(within(table).getByRole("rowheader", { name: label })).toBeInTheDocument();
    }
    expect(within(table).getByRole("row", { name: /p99/ })).toHaveTextContent(/52\s+ms/);
    expect(document.querySelector("[data-chart-canvas]")).toHaveAttribute("aria-hidden", "true");
  });

  it("adds the PHP line and its summary row only when the series has one", () => {
    const { rerender } = render(wrap(<LatencyBandChart series={latency} />));
    expect((instances[0]?.opts.series as unknown[]).length).toBe(4);
    rerender(wrap(<LatencyBandChart series={{ ...latency, php: [5, 6, 7, 8] }} />));
    const last = instances.at(-1);
    expect((last?.opts.series as unknown[]).length).toBe(5);
    expect(screen.getByRole("rowheader", { name: "PHP time" })).toBeInTheDocument();
  });

  it("stacks 4xx and 5xx cumulatively under the rps line", () => {
    render(
      wrap(
        <ThroughputErrorsChart
          series={{ t: T, rps: [5, 6, 7, 8], e4xx: [1, 1, 1, 1], e5xx: [2, 0, null, 1] }}
        />,
      ),
    );
    expect(instances[0]?.data).toEqual([T, [3, 1, 1, 2], [1, 1, 1, 1], [5, 6, 7, 8]]);
    expect(screen.getByRole("rowheader", { name: "5xx" })).toBeInTheDocument();
  });

  it("marks CPU/RAM as approximate and puts RAM on its own scale", () => {
    render(
      wrap(<ResourceChart series={{ t: T, cpu: [10, 20, 30, 40], ram: [1e8, 1e8, 2e8, 2e8] }} />),
    );
    expect(document.querySelector("[data-chart-approx]")).toHaveTextContent("≈");
    expect(screen.getAllByText(/^Estimate:/).length).toBeGreaterThan(0);
    const scales = instances[0]?.opts.scales as Record<string, unknown>;
    expect(Object.keys(scales)).toEqual(["x", "y", "ram"]);
  });

  it("renders the sparkline variant with a described image role", () => {
    render(wrap(<ChartSparkline t={T} values={[1, 2, 3, 4]} />));
    expect(
      screen.getByRole("img", { name: /Last 4 samples, latest 4, peak 4/ }),
    ).toBeInTheDocument();
    expect(instances).toHaveLength(1);
  });
});

describe("updates and theming", () => {
  it("appends in place with setData, without re-creating the chart", () => {
    const { rerender } = render(wrap(<LatencyBandChart series={latency} />));
    const first = instances[0];
    rerender(
      wrap(
        <LatencyBandChart
          series={{
            t: [...T, 1008],
            p50: [...latency.p50, 14],
            p90: [...latency.p90, 28],
            p99: [...latency.p99, 56],
          }}
        />,
      ),
    );
    expect(instances).toHaveLength(1);
    expect(first?.setData).toHaveBeenCalledTimes(1);
    expect(first?.destroy).not.toHaveBeenCalled();
  });

  it("reads colours from the tokens and re-creates the chart when the theme changes", async () => {
    document.documentElement.style.setProperty("--accent", "rebeccapurple");
    render(wrap(<LatencyBandChart series={latency} />));
    const strokeOf = (i: number) => {
      const series = instances[i]?.opts.series as { stroke?: string }[] | undefined;
      return series?.[1]?.stroke;
    };
    expect(strokeOf(0)).toBe("rebeccapurple");

    document.documentElement.style.setProperty("--accent", "teal");
    document.documentElement.dataset.theme = "dark";
    await waitFor(() => {
      expect(instances.length).toBeGreaterThan(1);
    });
    expect(instances[0]?.destroy).toHaveBeenCalled();
    expect(strokeOf(instances.length - 1)).toBe("teal");
  });
});

describe("markers", () => {
  it("renders visible markers as buttons reachable by Tab, with their tooltip", async () => {
    const user = userEvent.setup();
    render(wrap(<LatencyBandChart series={latency} markers={markers} />));
    const buttons = await screen.findAllByRole("button");
    // The marker out of the visible range is not drawn.
    expect(buttons).toHaveLength(2);
    expect(buttons[1]).toHaveAccessibleName(/PHP version changed: 8\.3 to 8\.4/);

    await user.tab();
    expect(buttons[0]).toHaveFocus();
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Restart");
    await user.tab();
    expect(buttons[1]).toHaveFocus();
  });
});

describe("markers arriving without a draw", () => {
  it("places markers when only the markers prop changes (no setData)", async () => {
    const { rerender } = render(wrap(<LatencyBandChart series={latency} markers={[]} />));
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    rerender(wrap(<LatencyBandChart series={latency} markers={markers} />));
    expect(await screen.findAllByRole("button")).toHaveLength(2);
    expect(instances).toHaveLength(1);
    expect(instances[0]?.setData).not.toHaveBeenCalled();

    // A refetch returning new objects with the same ids and a new detail updates the tooltip text.
    rerender(
      wrap(
        <LatencyBandChart
          series={latency}
          markers={markers.map((m) => (m.id === "b" ? { ...m, detail: "8.4 to 8.5" } : { ...m }))}
        />,
      ),
    );
    expect(await screen.findByRole("button", { name: /8\.4 to 8\.5/ })).toBeInTheDocument();
    expect(instances[0]?.setData).not.toHaveBeenCalled();
  });
});

describe("markers on live updates", () => {
  it("moves the handles with the x range by writing the DOM, keeping the same buttons", async () => {
    render(wrap(<LatencyBandChart series={latency} markers={markers} />));
    const before = await screen.findAllByRole("button");
    const slot = before[0]?.closest<HTMLElement>("[data-marker-id]");
    if (!slot) throw new Error("marker slot missing");
    const left0 = slot.style.left;
    const chart = instances[0] as unknown as {
      scales: { x: { min: number; max: number } };
      opts: { hooks: { draw: ((u: unknown) => void)[] } };
    };
    chart.scales.x = { min: 1000, max: 1020 };
    for (const hook of chart.opts.hooks.draw) hook(chart);
    expect(slot.style.left).not.toBe(left0);
    expect(screen.getAllByRole("button")[0]).toBe(before[0]);
  });
});

describe("legend", () => {
  const legendOf = () => {
    const legend = document.querySelector<HTMLElement>("[data-chart-legend]");
    if (!legend) throw new Error("legend missing");
    expect(legend).toHaveAttribute("aria-hidden", "true");
    return legend;
  };
  const swatches = (el: HTMLElement) =>
    [...el.querySelectorAll("[data-legend-swatch]")].map((n) => n.className);

  it("names each latency series with its latest value and its token swatch", () => {
    render(wrap(<LatencyBandChart series={{ ...latency, php: [5, 6, 7, 8] }} />));
    const legend = legendOf();
    for (const [name, value] of [
      ["p50", "13"],
      ["p90", "26"],
      ["p99", "52"],
      ["PHP time", "8"],
    ] as const) {
      const item = within(legend).getByText(name).closest("li");
      expect(item).toHaveTextContent(value);
    }
    const classes = swatches(legend);
    expect(classes).toHaveLength(4);
    expect(new Set(classes).size).toBe(4);
  });

  it("names rps, 4xx and 5xx in the throughput chart", () => {
    render(
      wrap(
        <ThroughputErrorsChart
          series={{ t: T, rps: [5, 6, 7, 8], e4xx: [1, 1, 1, 1], e5xx: [2, 0, null, 1] }}
        />,
      ),
    );
    const legend = legendOf();
    for (const name of ["Requests/s", "4xx", "5xx"]) {
      expect(within(legend).getByText(name)).toBeInTheDocument();
    }
    expect(new Set(swatches(legend)).size).toBe(3);
  });

  it("names CPU and RAM in the resource chart", () => {
    render(
      wrap(<ResourceChart series={{ t: T, cpu: [10, 20, 30, 40], ram: [1e8, 1e8, 2e8, 2e8] }} />),
    );
    const legend = legendOf();
    expect(within(legend).getByText("CPU").closest("li")).toHaveTextContent("40%");
    expect(within(legend).getByText("RAM")).toBeInTheDocument();
    expect(new Set(swatches(legend)).size).toBe(2);
  });

  it("gives the php-version marker a colour other than the p50 line", async () => {
    render(wrap(<LatencyBandChart series={latency} markers={markers} />));
    const marker = (await screen.findAllByRole("button")).find(
      (b) => b.dataset.markerKind === "php-version",
    );
    const p50 = document.querySelector("[data-chart-legend] [data-legend-swatch]");
    expect(marker?.className).toContain("bg-tool-4");
    expect(marker?.className).not.toContain("bg-accent");
    expect(p50?.className).toContain("bg-accent");
  });
});

describe("token-only colours", () => {
  it("has no colour literals in the chart sources", () => {
    const dir = __dirname;
    const sources = readdirSync(dir).filter((f) => /\.tsx?$/.test(f) && !f.includes(".test."));
    expect(sources.length).toBeGreaterThan(8);
    const literal = /#[0-9a-f]{3,8}\b|\b(?:rgba?|hsla?|oklch|oklab)\(/i;
    for (const file of sources) {
      const code = readFileSync(join(dir, file), "utf8");
      expect(literal.test(code), file).toBe(false);
    }
  });
});
