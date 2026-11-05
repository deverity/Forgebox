import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Slider } from "./Slider";

describe("Slider", () => {
  it("exposes slider semantics", () => {
    render(<Slider label="Memory" min={0} max={100} defaultValue={40} valueText="40 MB" />);
    const thumb = screen.getByRole("slider", { name: "Memory" });
    expect(thumb).toHaveAttribute("aria-valuenow", "40");
    expect(thumb).toHaveAttribute("aria-valuemin", "0");
    expect(thumb).toHaveAttribute("aria-valuemax", "100");
    expect(thumb).toHaveAttribute("aria-valuetext", "40 MB");
  });

  it("changes with arrows, Home and End", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(
      <Slider
        label="Memory"
        min={0}
        max={100}
        step={5}
        defaultValue={50}
        onValueChange={onChange}
      />,
    );
    await user.tab();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("slider")).toHaveAttribute("aria-valuenow", "55");
    await user.keyboard("{ArrowLeft}{ArrowLeft}");
    expect(screen.getByRole("slider")).toHaveAttribute("aria-valuenow", "45");
    await user.keyboard("{Home}");
    expect(screen.getByRole("slider")).toHaveAttribute("aria-valuenow", "0");
    await user.keyboard("{End}");
    expect(screen.getByRole("slider")).toHaveAttribute("aria-valuenow", "100");
    expect(onChange).toHaveBeenLastCalledWith(100);
  });

  it("is not focusable or changeable when disabled", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<Slider label="Memory" defaultValue={10} disabled onValueChange={onChange} />);
    await user.tab();
    await user.keyboard("{ArrowRight}");
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("slider")).toHaveAttribute("data-disabled");
  });
});
