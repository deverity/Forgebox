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

  it("loading blocks clicks, is aria-busy and keeps the text visible with a spinner", async () => {
    const onClick = vi.fn();
    render(
      <Button loading onClick={onClick}>
        Save changes
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Save changes" });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-busy", "true");
    expect(screen.getByTestId("button-spinner")).toBeInTheDocument();
    // The visible (loading) content is not transparent; the spinner sits in it, before the text.
    const active = screen.getByTestId("button-loading-content");
    expect(active).not.toHaveClass("opacity-0");
    expect(active).not.toHaveAttribute("aria-hidden");
    expect(active).toContainElement(screen.getByTestId("button-spinner"));
    expect(active).toHaveTextContent("Save changes");
    await userEvent.setup().click(button);
    expect(onClick).not.toHaveBeenCalled();
  });

  it("shows loadingLabel while loading and names the button by it", () => {
    const { rerender } = render(<Button loadingLabel="Saving">Save</Button>);
    expect(screen.getByRole("button", { name: "Save" })).not.toHaveAttribute("aria-busy");
    expect(screen.getByTestId("button-loading-content")).toHaveClass("opacity-0");
    rerender(
      <Button loading loadingLabel="Saving">
        Save
      </Button>,
    );
    expect(screen.getByRole("button", { name: "Saving" })).toHaveAttribute("aria-busy", "true");
    expect(screen.getByTestId("button-content")).toHaveClass("opacity-0");
    expect(screen.getByTestId("button-content")).toHaveAttribute("aria-hidden", "true");
    expect(screen.getByTestId("button-loading-content")).not.toHaveClass("opacity-0");
  });

  it("stacks normal and loading content in the same grid cell (width = max of both)", () => {
    render(
      <Button loading loadingLabel="Saving">
        Save
      </Button>,
    );
    for (const id of ["button-content", "button-loading-content"]) {
      const el = screen.getByTestId(id);
      expect(el).toHaveClass("col-start-1", "row-start-1");
      expect(el.parentElement).toHaveClass("grid");
    }
  });

  it("uses the progress cursor, not not-allowed, while loading", () => {
    const { rerender } = render(<Button loading>Go</Button>);
    expect(screen.getByRole("button").className).toContain("disabled:cursor-progress");
    expect(screen.getByRole("button").className).not.toContain("cursor-not-allowed");
    rerender(<Button disabled>Go</Button>);
    expect(screen.getByRole("button").className).toContain("disabled:cursor-not-allowed");
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
