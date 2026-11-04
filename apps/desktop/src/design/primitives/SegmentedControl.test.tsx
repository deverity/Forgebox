import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { mockReducedMotion } from "../../test/matchMedia";
import { SegmentedControl } from "./SegmentedControl";

const OPTIONS = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "system", label: "System" },
];

describe("SegmentedControl", () => {
  it("is a named radiogroup with the first option selected by default", () => {
    render(<SegmentedControl label="Theme" options={OPTIONS} />);
    expect(screen.getByRole("radiogroup", { name: "Theme" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
    expect(screen.getAllByRole("radio")).toHaveLength(3);
  });

  it("moves and selects with the arrow keys, wrapping around", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<SegmentedControl label="Theme" options={OPTIONS} onValueChange={onChange} />);
    await user.tab();
    // Radix moves focus in a timeout and selects while the arrow key is still held, so hold it.
    await user.keyboard("{ArrowRight>}");
    await waitFor(() => {
      expect(screen.getByRole("radio", { name: "Dark" })).toBeChecked();
    });
    await user.keyboard("{/ArrowRight}{ArrowLeft>}");
    await waitFor(() => {
      expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
    });
    await user.keyboard("{/ArrowLeft}{ArrowLeft>}");
    await waitFor(() => {
      expect(screen.getByRole("radio", { name: "System" })).toBeChecked();
    });
    await user.keyboard("{/ArrowLeft}");
    expect(onChange.mock.calls.map((call: unknown[]) => call[0])).toEqual([
      "dark",
      "light",
      "system",
    ]);
  });

  it("selects on click and shows a single selection pill", async () => {
    const user = userEvent.setup();
    render(<SegmentedControl label="Theme" options={OPTIONS} />);
    await user.click(screen.getByRole("radio", { name: "Dark" }));
    expect(screen.getByRole("radio", { name: "Dark" })).toBeChecked();
    expect(screen.getAllByTestId("segment-pill")).toHaveLength(1);
  });

  it("ignores interaction when disabled", async () => {
    const onChange = vi.fn();
    render(<SegmentedControl label="Theme" options={OPTIONS} disabled onValueChange={onChange} />);
    await userEvent.setup().click(screen.getByRole("radio", { name: "Dark" }));
    expect(onChange).not.toHaveBeenCalled();
  });

  it("uses the fade under reduced motion: no shared layout slide, opacity 0 to 1", async () => {
    mockReducedMotion(true);
    render(<SegmentedControl label="Theme" options={OPTIONS} />);
    expect(screen.getByRole("radiogroup")).toHaveAttribute("data-motion", "fade");
    const pill = screen.getByTestId("segment-pill");
    // Without layoutId Motion does not translate the pill between segments: it only fades.
    await waitFor(() => {
      expect(Number(pill.style.opacity)).toBeGreaterThan(0.99);
    });
    await userEvent.setup().click(screen.getByRole("radio", { name: "Dark" }));
    const next = screen.getByTestId("segment-pill");
    expect(next.style.transform === "" || next.style.transform === "none").toBe(true);
  });
});

describe("SegmentedControl pill placement (R-02)", () => {
  it("follows the checked item's offsets inside the group, whatever is above the page", async () => {
    // jsdom has no layout: give each radio the box a browser would report relative to the group.
    const offsets: Record<string, [number, number, number, number]> = {
      System: [2, 2, 60, 24],
      Light: [64, 2, 52, 24],
      Dark: [118, 2, 48, 24],
    };
    const props = ["offsetLeft", "offsetTop", "offsetWidth", "offsetHeight"] as const;
    const originals = props.map((prop) =>
      Object.getOwnPropertyDescriptor(HTMLElement.prototype, prop),
    );
    props.forEach((prop, index) => {
      Object.defineProperty(HTMLElement.prototype, prop, {
        configurable: true,
        get(this: HTMLElement) {
          return (offsets[this.textContent] as number[] | undefined)?.[index] ?? 0;
        },
      });
    });
    try {
      mockReducedMotion(true);
      render(<SegmentedControl label="Theme" options={OPTIONS} value="light" />);
      const pill = await screen.findByTestId("segment-pill");
      expect(pill).toHaveStyle({ left: "64px", top: "2px", width: "52px", height: "24px" });
      // The pill is positioned in the group's own box, never from the page position.
      expect(screen.getByRole("radiogroup").className).toContain("relative");
    } finally {
      props.forEach((prop, index) => {
        const original = originals[index];
        if (original !== undefined) Object.defineProperty(HTMLElement.prototype, prop, original);
      });
    }
  });
});
