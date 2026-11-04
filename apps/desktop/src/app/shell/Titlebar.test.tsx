// The content toolbar and the window controls (M2-E1-T23, docs/07 section 4, ADR-0035 addendum).
import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { OPEN_PALETTE_EVENT, Titlebar } from "./Titlebar";
import { WINDOW_CONTROL_COLOR, WINDOW_CONTROL_ORDER } from "./WindowControls";
import type { WindowApi } from "./windowApi";

// The visible label is the accessible name (WCAG 2.5.3), the shortcut is aria-keyshortcuts
// and the hint is the tooltip.
const PALETTE = "Search";
const PALETTE_HINT = "Open the command palette (Ctrl+K)";

function fakeApi({ maximized = false, focused = true } = {}) {
  let notifyMax: ((value: boolean) => void) | undefined;
  let notifyFocus: ((value: boolean) => void) | undefined;
  const api = {
    minimize: vi.fn(() => Promise.resolve()),
    toggleMaximize: vi.fn(() => Promise.resolve()),
    close: vi.fn(() => Promise.resolve()),
    isMaximized: vi.fn(() => Promise.resolve(maximized)),
    onMaximizedChange: vi.fn((cb: (value: boolean) => void) => {
      notifyMax = cb;
      return Promise.resolve(() => {
        notifyMax = undefined;
      });
    }),
    isFocused: vi.fn(() => Promise.resolve(focused)),
    onFocusChange: vi.fn((cb: (value: boolean) => void) => {
      notifyFocus = cb;
      return Promise.resolve(() => {
        notifyFocus = undefined;
      });
    }),
  } satisfies WindowApi;
  return {
    api,
    setMaximized: (value: boolean) => notifyMax?.(value),
    setFocused: (value: boolean) => notifyFocus?.(value),
  };
}

describe("content toolbar", () => {
  it("is a 44 px deep drag region with the page title on the left", () => {
    render(<Titlebar title="Dashboard" windowApi={fakeApi().api} />);
    const bar = screen.getByTestId("titlebar");
    expect(bar.tagName).toBe("HEADER");
    expect(bar).toHaveAttribute("data-tauri-drag-region", "deep");
    expect(bar).toHaveClass("h-11");
    // The title is plain text (the page keeps its own heading), first in the row.
    const title = screen.getByTestId("toolbar-title");
    expect(title).toHaveTextContent("Dashboard");
    expect(title.tagName).toBe("SPAN");
    expect(bar.firstElementChild).toBe(title);
    // The product name moved to the sidebar.
    expect(bar).not.toHaveTextContent("Forgebox");
  });

  it("puts search, the primary action and the window controls on the right, in that order", () => {
    render(
      <Titlebar
        title="Dashboard"
        windowApi={fakeApi().api}
        primaryAction={<button type="button">Start all</button>}
      />,
    );
    const bar = screen.getByTestId("titlebar");
    const search = screen.getByRole("button", { name: PALETTE });
    const action = screen.getByTestId("toolbar-action");
    const controls = screen.getByRole("group", { name: "Window controls" });
    const order = [...bar.children];
    expect(order.indexOf(search)).toBeLessThan(order.indexOf(action));
    expect(order.indexOf(action)).toBeLessThan(order.indexOf(controls));
    expect(bar.lastElementChild).toBe(controls);
  });

  it("has no action slot without a primary action", () => {
    render(<Titlebar windowApi={fakeApi().api} />);
    expect(screen.queryByTestId("toolbar-action")).toBeNull();
  });

  it("reaches every control with Tab, in order", async () => {
    const user = userEvent.setup();
    render(<Titlebar windowApi={fakeApi().api} />);
    await user.tab();
    expect(screen.getByRole("button", { name: PALETTE })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("button", { name: "Minimize" })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("button", { name: "Maximize" })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("button", { name: "Close" })).toHaveFocus();
  });

  it("opens the palette through the prop or the default event", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    const { unmount } = render(<Titlebar windowApi={fakeApi().api} onOpenPalette={onOpen} />);
    const search = screen.getByRole("button", { name: PALETTE });
    expect(search).toHaveAttribute("aria-keyshortcuts", "Control+K");
    expect(search).toHaveAttribute("title", PALETTE_HINT);
    expect(search).toHaveTextContent("Ctrl+K");
    // Soft field: low-alpha fill, no outline.
    expect(search).toHaveClass("bg-fill", "border-stroke-control");
    await user.click(search);
    expect(onOpen).toHaveBeenCalledOnce();
    unmount();

    const listener = vi.fn();
    window.addEventListener(OPEN_PALETTE_EVENT, listener);
    render(<Titlebar windowApi={fakeApi().api} />);
    await user.click(screen.getByRole("button", { name: PALETTE }));
    expect(listener).toHaveBeenCalledOnce();
    window.removeEventListener(OPEN_PALETTE_EVENT, listener);
  });

  it("lets the palette task replace the search slot", () => {
    render(<Titlebar windowApi={fakeApi().api} searchSlot={<span>slot</span>} />);
    expect(screen.getByText("slot")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: PALETTE })).toBeNull();
    expect(screen.queryByTitle(PALETTE_HINT)).toBeNull();
  });
});

describe("window controls (coloured dots)", () => {
  it("are minimize, maximize, close in Windows order with token colours", () => {
    render(<Titlebar windowApi={fakeApi().api} />);
    const group = screen.getByRole("group", { name: "Window controls" });
    const buttons = within(group).getAllByRole("button");
    expect(buttons.map((b) => b.getAttribute("aria-label"))).toEqual([
      "Minimize",
      "Maximize",
      "Close",
    ]);
    expect(buttons.map((b) => b.dataset.control)).toEqual([...WINDOW_CONTROL_ORDER]);
    expect(WINDOW_CONTROL_COLOR).toEqual({
      minimize: "bg-window-minimize",
      maximize: "bg-window-maximize",
      close: "bg-window-close",
    });
    for (const id of WINDOW_CONTROL_ORDER) {
      const dot = screen.getByTestId(`dot-${id}`);
      expect(dot).toHaveClass(WINDOW_CONTROL_COLOR[id], "size-3", "rounded-full");
      // Thin inner stroke from a token, grey while the window is inactive.
      expect(dot).toHaveClass("ring-inset", "ring-window-dot-stroke");
      expect(dot.className).toContain("group-data-[focused=false]/dots:bg-window-inactive");
    }
  });

  it("have tooltips, 24x24 hit targets and a visible focus ring", () => {
    render(<Titlebar windowApi={fakeApi().api} />);
    for (const name of ["Minimize", "Maximize", "Close"]) {
      const button = screen.getByRole("button", { name });
      expect(button).toHaveAttribute("title", name);
      expect(button).toHaveClass(
        "size-6",
        "focus-visible:outline-solid",
        "focus-visible:outline-accent",
      );
    }
  });

  it("show the glyphs only while the group is hovered or focused", () => {
    render(<Titlebar windowApi={fakeApi().api} />);
    const group = screen.getByRole("group", { name: "Window controls" });
    expect(group).toHaveClass("group/dots");
    for (const glyph of group.querySelectorAll("svg")) {
      const cls = glyph.getAttribute("class") ?? "";
      expect(cls).toContain("opacity-0");
      expect(cls).toContain("group-hover/dots:opacity-100");
      expect(cls).toContain("group-has-[:focus-visible]/dots:opacity-100");
      // Inactive window: grey dots without glyphs, as in the mockup.
      expect(cls).toContain("group-data-[focused=false]/dots:opacity-0");
      expect(glyph).toHaveAttribute("aria-hidden", "true");
    }
  });

  it("turn grey when the window loses focus and back when it regains it", async () => {
    const { api, setFocused } = fakeApi({ focused: false });
    render(<Titlebar windowApi={api} />);
    const group = screen.getByTestId("window-controls");
    await vi.waitFor(() => {
      expect(group).toHaveAttribute("data-focused", "false");
    });
    await vi.waitFor(() => {
      expect(api.onFocusChange).toHaveBeenCalled();
    });
    act(() => {
      setFocused(true);
    });
    expect(group).toHaveAttribute("data-focused", "true");
    act(() => {
      setFocused(false);
    });
    expect(group).toHaveAttribute("data-focused", "false");
  });

  it("act on the window with click, Enter and Space", async () => {
    const user = userEvent.setup();
    const { api } = fakeApi();
    render(<Titlebar windowApi={api} />);
    await user.click(screen.getByRole("button", { name: "Minimize" }));
    await user.click(screen.getByRole("button", { name: "Maximize" }));
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(api.minimize).toHaveBeenCalledOnce();
    expect(api.toggleMaximize).toHaveBeenCalledOnce();
    expect(api.close).toHaveBeenCalledOnce();

    screen.getByRole("button", { name: "Maximize" }).focus();
    await user.keyboard("{Enter}");
    await user.keyboard(" ");
    expect(api.toggleMaximize).toHaveBeenCalledTimes(3);
  });

  it("say Restore while maximized and follow resizes", async () => {
    const { api, setMaximized } = fakeApi({ maximized: true });
    const { unmount } = render(<Titlebar windowApi={api} />);
    expect(await screen.findByRole("button", { name: "Restore" })).toHaveAttribute(
      "data-maximized",
      "true",
    );
    await vi.waitFor(() => {
      expect(api.onMaximizedChange).toHaveBeenCalled();
    });
    act(() => {
      setMaximized(false);
    });
    expect(await screen.findByRole("button", { name: "Maximize" })).toBeInTheDocument();
    unmount();
  });
});
