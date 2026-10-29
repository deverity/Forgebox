import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Select } from "./Select";

const OPTIONS = [
  { value: "8.4", label: "PHP 8.4" },
  { value: "8.3", label: "PHP 8.3" },
  { value: "7.4", label: "PHP 7.4", disabled: true },
];

describe("Select", () => {
  it("exposes a combobox trigger with the placeholder", () => {
    render(<Select label="Version" options={OPTIONS} placeholder="Pick one" />);
    const trigger = screen.getByRole("combobox", { name: "Version" });
    expect(trigger).toHaveTextContent("Pick one");
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("opens with Enter, moves with arrows, selects with Enter and closes", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(<Select label="Version" options={OPTIONS} onValueChange={onChange} />);
    await user.tab();
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("listbox")).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(3);
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onChange).toHaveBeenCalledWith("8.3");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.getByRole("combobox")).toHaveTextContent("PHP 8.3");
  });

  it("closes with Escape without changing the value", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(
      <Select label="Version" options={OPTIONS} defaultValue="8.4" onValueChange={onChange} />,
    );
    await user.tab();
    await user.keyboard("{Enter}");
    await screen.findByRole("listbox");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("marks disabled options and a disabled trigger", async () => {
    const user = userEvent.setup();
    const { rerender } = render(<Select label="Version" options={OPTIONS} />);
    await user.tab();
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("option", { name: "PHP 7.4" })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    await user.keyboard("{Escape}");
    rerender(<Select label="Version" options={OPTIONS} disabled />);
    expect(screen.getByRole("combobox")).toBeDisabled();
  });
});
