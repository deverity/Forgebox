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

  it("uses the fade under reduced motion", () => {
    mockReducedMotion(true);
    render(<SegmentedControl label="Theme" options={OPTIONS} />);
    expect(screen.getByRole("radiogroup")).toHaveAttribute("data-motion", "fade");
  });
});
