import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
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
});
