import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { conflictingColorClasses } from "./shared";
import { TextField } from "./TextField";

const validate = (v: string) => (/^[a-z0-9-]+$/.test(v) ? undefined : "Invalid host");

describe("TextField", () => {
  it("associates the label, hint and input", () => {
    render(<TextField label="Host" hint="Lowercase only" />);
    const input = screen.getByRole("textbox", { name: "Host" });
    expect(input).toHaveAccessibleDescription("Lowercase only");
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it("validates inline: silent before the first blur, then live", async () => {
    const user = userEvent.setup();
    render(<TextField label="Host" validate={validate} />);
    const input = screen.getByRole("textbox");
    await user.type(input, "Bad Host");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    await user.tab();
    expect(screen.getByRole("alert")).toHaveTextContent("Invalid host");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription("Invalid host");
    await user.clear(input);
    await user.type(input, "good-host");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it("shows a forced error and calls onChange", async () => {
    const onChange = vi.fn();
    render(<TextField label="Port" defaultValue="80" error="In use" onChange={onChange} />);
    expect(screen.getByRole("alert")).toHaveTextContent("In use");
    await userEvent.setup().type(screen.getByRole("textbox"), "1");
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("textbox")).toHaveValue("801");
  });

  it("is not editable when disabled", async () => {
    render(<TextField label="Host" defaultValue="app" disabled />);
    await userEvent.setup().type(screen.getByRole("textbox"), "x");
    expect(screen.getByRole("textbox")).toBeDisabled();
    expect(screen.getByRole("textbox")).toHaveValue("app");
  });

  it("paints the error border: state-failed and NOT stroke-strong", () => {
    render(<TextField label="Port" error="In use" />);
    const input = screen.getByRole("textbox");
    expect(input).toHaveClass("border-state-failed");
    expect(input).not.toHaveClass("border-stroke-strong");
    expect(conflictingColorClasses(input.className)).toEqual([]);
  });

  it("uses the neutral border without error and has no conflicting colours", () => {
    render(<TextField label="Port" />);
    const input = screen.getByRole("textbox");
    expect(input).toHaveClass("border-stroke-strong");
    expect(input).not.toHaveClass("border-state-failed");
  });

  it("sizes by width token", () => {
    const { container } = render(<TextField label="Port" width="sm" />);
    expect(container.firstElementChild).toHaveClass("w-40");
  });
});
