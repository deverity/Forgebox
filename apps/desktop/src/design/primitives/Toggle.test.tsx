import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { mockReducedMotion } from "../../test/matchMedia";
import { Toggle } from "./Toggle";

describe("Toggle", () => {
  it("is a switch named by its label", () => {
    render(<Toggle label="Autostart" />);
    expect(screen.getByRole("switch", { name: "Autostart" })).toHaveAttribute(
      "aria-checked",
      "false",
    );
  });

  it("toggles with click and Space and reports the value", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<Toggle label="Autostart" onCheckedChange={onChange} />);
    const toggle = screen.getByRole("switch");
    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-checked", "true");
    await user.keyboard(" ");
    expect(toggle).toHaveAttribute("aria-checked", "false");
    expect(onChange.mock.calls).toEqual([[true], [false]]);
  });

  it("follows the controlled value", async () => {
    const user = userEvent.setup();
    render(<Toggle label="Autostart" checked />);
    await user.click(screen.getByRole("switch"));
    expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "true");
  });

  it("does nothing when disabled", async () => {
    const onChange = vi.fn();
    render(<Toggle label="Autostart" disabled onCheckedChange={onChange} />);
    await userEvent.setup().click(screen.getByRole("switch"));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("switch")).toBeDisabled();
  });

  it("uses the spring normally and the 100 ms fade under reduced motion", () => {
    const { unmount } = render(<Toggle label="A" />);
    expect(screen.getByRole("switch")).toHaveAttribute("data-motion", "spring");
    expect(screen.getByRole("switch")).toHaveStyle({ transitionDuration: "var(--duration-micro)" });
    unmount();
    mockReducedMotion(true);
    render(<Toggle label="A" />);
    expect(screen.getByRole("switch")).toHaveAttribute("data-motion", "fade");
    expect(screen.getByRole("switch")).toHaveStyle({
      transitionDuration: "var(--duration-reduced)",
    });
  });
});
