import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BUTTON_VARIANTS, Button } from "./Button";
import { IconButton } from "./IconButton";

describe("Button", () => {
  it.each(BUTTON_VARIANTS)("renders the %s variant as a button", (variant) => {
    render(<Button variant={variant}>Go</Button>);
    expect(screen.getByRole("button", { name: "Go" })).toHaveAttribute("data-variant", variant);
  });

  it("activates with click, Enter and Space", async () => {
    const onClick = vi.fn();
    const user = userEvent.setup();
    render(<Button onClick={onClick}>Go</Button>);
    await user.click(screen.getByRole("button"));
    await user.keyboard("{Enter}");
    await user.keyboard(" ");
    expect(onClick).toHaveBeenCalledTimes(3);
  });

  it("does not fire when disabled", async () => {
    const onClick = vi.fn();
    render(
      <Button disabled onClick={onClick}>
        Go
      </Button>,
    );
    await userEvent.setup().click(screen.getByRole("button"));
    expect(onClick).not.toHaveBeenCalled();
    expect(screen.getByRole("button")).toBeDisabled();
  });

  it("loading blocks clicks, is aria-busy and keeps the label in flow to hold the width", async () => {
    const onClick = vi.fn();
    render(
      <Button loading onClick={onClick}>
        Save changes
      </Button>,
    );
    // The label must stay in the accessibility tree (no visibility:hidden).
    const button = screen.getByRole("button", { name: "Save changes" });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-busy", "true");
    expect(screen.getByTestId("button-spinner")).toBeInTheDocument();
    // Still in the DOM, in layout and in the a11y tree; only transparent.
    expect(screen.getByText("Save changes")).not.toHaveClass("invisible");
    await userEvent.setup().click(button);
    expect(onClick).not.toHaveBeenCalled();
  });
});

describe("IconButton", () => {
  it("is named by its label and activates with the keyboard", async () => {
    const onClick = vi.fn();
    const user = userEvent.setup();
    render(<IconButton label="Refresh" icon={<svg />} onClick={onClick} />);
    await user.tab();
    expect(screen.getByRole("button", { name: "Refresh" })).toHaveFocus();
    await user.keyboard("{Enter}");
    await user.keyboard(" ");
    expect(onClick).toHaveBeenCalledTimes(2);
  });

  it("supports disabled and loading", () => {
    const { rerender } = render(<IconButton label="Refresh" icon={<svg />} disabled />);
    expect(screen.getByRole("button")).toBeDisabled();
    rerender(<IconButton label="Refresh" icon={<svg />} loading />);
    expect(screen.getByRole("button")).toHaveAttribute("aria-busy", "true");
    expect(screen.getByRole("button")).toBeDisabled();
  });
});
