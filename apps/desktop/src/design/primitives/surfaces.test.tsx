import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Menu } from "./Menu";
import { Popover } from "./Popover";
import { Sheet } from "./Sheet";
import { mockReducedMotion, restoreMotion } from "./testUtils";

restoreMotion();

describe("Sheet", () => {
  function setup() {
    return render(
      <Sheet trigger={<button type="button">open sheet</button>} title="Details">
        <button type="button">first</button>
        <button type="button">second</button>
      </Sheet>,
    );
  }

  it("is 420 px wide and traps focus while open", async () => {
    const user = userEvent.setup();
    setup();
    await user.click(screen.getByRole("button", { name: "open sheet" }));
    const dialog = await screen.findByRole("dialog", { name: "Details" });
    expect(dialog.className).toContain("w-[420px]");
    for (let i = 0; i < 6; i += 1) {
      await user.tab();
      expect(dialog.contains(document.activeElement)).toBe(true);
    }
  });

  it("closes on Escape and returns focus to the trigger", async () => {
    const user = userEvent.setup();
    setup();
    const trigger = screen.getByRole("button", { name: "open sheet" });
    await user.click(trigger);
    await screen.findByRole("dialog");
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(trigger).toHaveFocus();
  });

  it("uses the 100 ms opacity-only fade under reduced motion", async () => {
    mockReducedMotion(true);
    const user = userEvent.setup();
    setup();
    await user.click(screen.getByRole("button", { name: "open sheet" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveAttribute("data-motion", "reduced");
  });

  it("uses the full slide otherwise", async () => {
    mockReducedMotion(false);
    const user = userEvent.setup();
    setup();
    await user.click(screen.getByRole("button", { name: "open sheet" }));
    expect(await screen.findByRole("dialog")).toHaveAttribute("data-motion", "full");
  });
});

describe("Popover", () => {
  function setup() {
    return render(
      <Popover trigger={<button type="button">open pop</button>} label="Filters">
        <button type="button">inside a</button>
        <button type="button">inside b</button>
      </Popover>,
    );
  }

  it("has radius 12 (rounded-sheet), traps focus, closes on Escape, restores focus", async () => {
    const user = userEvent.setup();
    setup();
    const trigger = screen.getByRole("button", { name: "open pop" });
    await user.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "Filters" });
    expect(dialog.className).toContain("rounded-sheet");
    for (let i = 0; i < 5; i += 1) {
      await user.tab();
      expect(dialog.contains(document.activeElement)).toBe(true);
    }
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(trigger).toHaveFocus();
  });

  it("uses the 100 ms fade under reduced motion", async () => {
    mockReducedMotion(true);
    const user = userEvent.setup();
    setup();
    await user.click(screen.getByRole("button", { name: "open pop" }));
    expect(await screen.findByRole("dialog")).toHaveAttribute("data-motion", "reduced");
  });
});

describe("Menu", () => {
  function setup(onSelect = vi.fn()) {
    render(
      <Menu
        entries={[
          { id: "a", label: "Restart", onSelect },
          { type: "separator", id: "s" },
          { id: "b", label: "Remove", onSelect: vi.fn(), destructive: true },
        ]}
      >
        <span>row</span>
      </Menu>,
    );
    return onSelect;
  }

  it("opens on context menu, moves with arrows, activates, and closes", async () => {
    const user = userEvent.setup();
    const onSelect = setup();
    fireEvent.contextMenu(screen.getByText("row"));
    const menu = await screen.findByRole("menu");
    expect(within(menu).getAllByRole("menuitem")).toHaveLength(2);
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onSelect).toHaveBeenCalledTimes(1);
    await waitFor(() => {
      expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    });
  });

  it("closes on Escape and returns focus to its trigger", async () => {
    const user = userEvent.setup();
    setup();
    const trigger = screen.getByRole("group");
    trigger.focus();
    fireEvent.contextMenu(trigger);
    await screen.findByRole("menu");
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    });
    expect(trigger).toHaveFocus();
  });

  it("uses the 100 ms fade under reduced motion", async () => {
    mockReducedMotion(true);
    setup();
    fireEvent.contextMenu(screen.getByText("row"));
    expect(await screen.findByRole("menu")).toHaveAttribute("data-motion", "reduced");
  });
});
