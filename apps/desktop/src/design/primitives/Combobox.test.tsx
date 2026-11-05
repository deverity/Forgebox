import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Combobox } from "./Combobox";

const OPTIONS = [
  { value: "app.test", label: "app.test" },
  { value: "blog.test", label: "blog.test" },
  { value: "shop.test", label: "shop.test" },
];

function setup(props: Partial<Parameters<typeof Combobox>[0]> = {}) {
  return render(<Combobox label="Site" options={OPTIONS} emptyText="No site" {...props} />);
}

describe("Combobox", () => {
  it("has the combobox/listbox ARIA wiring", () => {
    setup();
    const input = screen.getByRole("combobox", { name: "Site" });
    expect(input).toHaveAttribute("aria-expanded", "false");
    expect(input).toHaveAttribute("aria-autocomplete", "list");
    expect(input.getAttribute("aria-controls")).toBeTruthy();
  });

  it("opens with ArrowDown, navigates with arrows and selects with Enter", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    setup({ onValueChange: onChange });
    await user.tab();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("combobox")).toHaveAttribute("aria-expanded", "true");
    expect(screen.getAllByRole("option")).toHaveLength(3);
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onChange).toHaveBeenCalledWith("blog.test");
    expect(screen.getByRole("combobox")).toHaveValue("blog.test");
    expect(screen.getByRole("combobox")).toHaveAttribute("aria-expanded", "false");
  });

  it("tracks the active option with aria-activedescendant", async () => {
    const user = userEvent.setup();
    setup();
    await user.tab();
    await user.keyboard("{ArrowDown}{ArrowDown}");
    const active = screen.getByRole("combobox").getAttribute("aria-activedescendant");
    expect(screen.getByRole("option", { name: "blog.test" })).toHaveAttribute("id", active);
  });

  it("filters while typing and shows the empty text", async () => {
    const user = userEvent.setup();
    setup();
    await user.type(screen.getByRole("combobox"), "sho");
    expect(screen.getAllByRole("option")).toHaveLength(1);
    await user.type(screen.getByRole("combobox"), "zzz");
    expect(screen.queryAllByRole("option")).toHaveLength(0);
    expect(screen.getByText("No site")).toBeInTheDocument();
  });

  it("closes with Escape and restores the selected label", async () => {
    const user = userEvent.setup();
    setup({ defaultValue: "app.test" });
    const input = screen.getByRole("combobox");
    await user.click(input);
    await user.keyboard("zz");
    expect(input).toHaveAttribute("aria-expanded", "true");
    await user.keyboard("{Escape}");
    expect(input).toHaveAttribute("aria-expanded", "false");
    expect(input).toHaveValue("app.test");
  });

  it("selects with the mouse and respects disabled", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    const { unmount } = setup({ onValueChange: onChange });
    await user.click(screen.getByRole("combobox"));
    await user.keyboard("{ArrowDown}");
    await user.click(screen.getByRole("option", { name: "shop.test" }));
    expect(onChange).toHaveBeenCalledWith("shop.test");
    unmount();
    setup({ disabled: true });
    expect(screen.getByRole("combobox")).toBeDisabled();
  });
});
