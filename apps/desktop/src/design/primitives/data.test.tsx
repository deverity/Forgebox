import { useState } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Counts Sparkline renders: it calls useFormatters exactly once per render.
const formatterCalls = vi.hoisted(() => ({ count: 0 }));
vi.mock("./useFormatters", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./useFormatters")>();
  return {
    useFormatters: () => {
      formatterCalls.count += 1;
      return actual.useFormatters();
    },
  };
});
import { Card } from "./Card";
import { EmptyState } from "./EmptyState";
import { ListRow } from "./ListRow";
import { Sparkline, sparklinePoints, SPARKLINE_SAMPLES } from "./Sparkline";
import { VirtualList } from "./VirtualList";

describe("Card", () => {
  it("has border and surface but no shadow", () => {
    render(<Card data-testid="c">x</Card>);
    const cls = screen.getByTestId("c").className;
    expect(cls).toContain("border");
    expect(cls).toContain("bg-surface");
    expect(cls).not.toMatch(/shadow/);
  });
});

describe("ListRow", () => {
  it("is a button when clickable and reports selection", () => {
    const onClick = vi.fn();
    render(<ListRow title="nginx" subtitle="1.27" trailing="80" selected onClick={onClick} />);
    const row = screen.getByRole("button", { name: /nginx/ });
    expect(row).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(row);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("is static without onClick and always 48 px tall", () => {
    const { container } = render(<ListRow title="static" />);
    expect(screen.queryByRole("button")).toBeNull();
    expect((container.firstElementChild as HTMLElement).className).toContain("h-12");
  });
});

describe("VirtualList", () => {
  // jsdom has no layout: give every element the 480 px viewport the virtualizer reads.
  beforeEach(() => {
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get: () => 480,
    });
    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get: () => 400,
    });
  });
  afterEach(() => {
    delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
    delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
  });

  const items = Array.from({ length: 500 }, (_, i) => ({
    id: `item-${String(i)}`,
    name: `Row ${String(i)}`,
  }));

  it("mounts only the visible window out of 500 items", () => {
    render(
      <VirtualList
        label="Sites"
        items={items}
        getKey={(i) => i.id}
        renderRow={(i) => <ListRow title={i.name} />}
        initialHeight={480}
      />,
    );
    const rows = screen.getAllByRole("listitem");
    expect(rows.length).toBeGreaterThan(5);
    expect(rows.length).toBeLessThan(40);
    expect(screen.getByText("Row 0")).toBeInTheDocument();
    expect(screen.queryByText("Row 499")).toBeNull();
    expect(rows[0]).toHaveAttribute("aria-setsize", "500");
  });

  it("renders an empty list without rows", () => {
    render(<VirtualList label="Sites" items={[]} getKey={() => "x"} renderRow={() => null} />);
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });
});

describe("Sparkline", () => {
  it("draws at most 60 samples and exposes tnum labels", () => {
    const samples = Array.from({ length: 100 }, (_, i) => i);
    const { container } = render(<Sparkline samples={samples} />);
    const points = container.querySelector("polyline")?.getAttribute("points") ?? "";
    expect(points.split(" ")).toHaveLength(SPARKLINE_SAMPLES);
    const labels = container.querySelector("[data-part=labels]");
    expect(labels?.className).toContain("tabular-nums");
    expect(container.querySelector("[data-part=latest]")?.textContent).toBe("99");
    expect(container.querySelector("[data-part=peak]")?.textContent).toBe("99");
    expect(screen.getByRole("img").getAttribute("aria-label")).toContain("latest 99");
  });

  it("maps values into the viewbox (peak at the top, zero at the bottom)", () => {
    expect(sparklinePoints([0, 10], 100, 20, 10)).toBe("2.0,18.0 98.0,2.0");
    expect(sparklinePoints([], 100, 20, 10)).toBe("");
  });

  it("does not re-render when a sibling updates or the parent re-renders with equal data", () => {
    function Panel() {
      const [a, setA] = useState([1, 2, 3]);
      const [b] = useState([4, 5, 6]);
      return (
        <div>
          <button
            type="button"
            onClick={() => {
              setA((prev) => [...prev, prev.length + 1]);
            }}
          >
            tick
          </button>
          <Sparkline samples={a} />
          {/* fresh array with equal values on every parent render */}
          <Sparkline samples={[...b]} />
        </div>
      );
    }
    formatterCalls.count = 0;
    render(<Panel />);
    expect(formatterCalls.count).toBe(2);
    fireEvent.click(screen.getByRole("button"));
    fireEvent.click(screen.getByRole("button"));
    // Only the first sparkline (whose data changed) re-rendered, twice; the second never did.
    expect(formatterCalls.count).toBe(4);
  });
});

describe("EmptyState", () => {
  it("renders illustration slot, text and action", () => {
    const onClick = vi.fn();
    const { container } = render(
      <EmptyState
        title="No sites yet"
        description="Create your first site."
        illustration={<svg data-testid="art" />}
        action={
          <button type="button" onClick={onClick}>
            New site
          </button>
        }
      />,
    );
    expect(screen.getByRole("heading", { name: "No sites yet" })).toBeInTheDocument();
    expect(container.querySelector("[data-slot=illustration]")).toContainElement(
      screen.getByTestId("art"),
    );
    fireEvent.click(screen.getByRole("button", { name: "New site" }));
    expect(onClick).toHaveBeenCalled();
  });
});
