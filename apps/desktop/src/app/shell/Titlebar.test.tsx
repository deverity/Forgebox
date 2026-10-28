import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { OPEN_PALETTE_EVENT, Titlebar } from "./Titlebar";
import type { WindowApi } from "./windowApi";

const PALETTE = "Open the command palette (Ctrl+K)";

function fakeApi(maximized = false) {
  let notify: ((value: boolean) => void) | undefined;
  const api = {
    minimize: vi.fn(() => Promise.resolve()),
    toggleMaximize: vi.fn(() => Promise.resolve()),
    close: vi.fn(() => Promise.resolve()),
    isMaximized: vi.fn(() => Promise.resolve(maximized)),
    onMaximizedChange: vi.fn((cb: (value: boolean) => void) => {
      notify = cb;
      return Promise.resolve(() => {
        notify = undefined;
      });
    }),
  } satisfies WindowApi;
  return { api, setMaximized: (value: boolean) => notify?.(value) };
}

describe("Titlebar", () => {
  it("is a deep drag region with the product name", () => {
    render(<Titlebar windowApi={fakeApi().api} />);
    const bar = screen.getByTestId("titlebar");
    expect(bar.tagName).toBe("HEADER");
    expect(bar).toHaveAttribute("data-tauri-drag-region", "deep");
    expect(bar).toHaveTextContent("Forgebox");
  });

  it("has accessible window controls with tooltips that act on the window", async () => {
    const user = userEvent.setup();
    const { api } = fakeApi();
    render(<Titlebar windowApi={api} />);
    expect(screen.getByRole("group", { name: "Window controls" })).toBeInTheDocument();
    for (const name of ["Minimize", "Maximize", "Close"]) {
      expect(screen.getByRole("button", { name })).toHaveAttribute("title", name);
    }
    await user.click(screen.getByRole("button", { name: "Minimize" }));
    await user.click(screen.getByRole("button", { name: "Maximize" }));
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(api.minimize).toHaveBeenCalledOnce();
    expect(api.toggleMaximize).toHaveBeenCalledOnce();
    expect(api.close).toHaveBeenCalledOnce();
  });

  it("shows Restore while maximized and follows resizes", async () => {
    const { api, setMaximized } = fakeApi(true);
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
    expect(search).toHaveTextContent("Ctrl+K");
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
  });
});
