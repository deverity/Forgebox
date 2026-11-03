// Acceptance of M2-E1-T14: the palette, the shortcut registry in the running app and the global
// terminal shortcut plumbing.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLanguage } from "../../i18n";
import { toastStore } from "../../design/primitives/toastStore";
import { renderApp } from "../testApp";
import { DEFAULT_SHORTCUTS, shortcutRegistry } from "../shortcuts/registry";
import type { GlobalInvoke } from "../shortcuts/useShortcuts";
import { globalShortcutBackend } from "./PaletteHost";

beforeEach(() => {
  globalThis.localStorage.clear();
});

afterEach(async () => {
  globalShortcutBackend.invoke = null;
  for (const def of DEFAULT_SHORTCUTS) await shortcutRegistry.reset(def.id);
  globalThis.localStorage.clear();
  await act(async () => {
    await setLanguage("en-US");
  });
});

function pathOf(h: Awaited<ReturnType<typeof renderApp>>): string {
  return h.router.state.location.pathname;
}

async function openPalette(user: ReturnType<typeof userEvent.setup>) {
  await user.keyboard("{Control>}k{/Control}");
  return within(await screen.findByRole("dialog")).getByRole("combobox");
}

describe("palette", () => {
  it("Ctrl+K opens and focuses it, Escape closes and restores the focus", async () => {
    const user = userEvent.setup();
    await renderApp();
    const link = await screen.findByRole("link", { name: /^Sites/ });
    link.focus();
    const input = await openPalette(user);
    await waitFor(() => {
      expect(input).toHaveFocus();
    });
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    // Radix returns the focus on the next tick.
    await waitFor(() => {
      expect(link).toHaveFocus();
    });
  });

  it("opens from the titlebar button too", async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(await screen.findByRole("button", { name: "Search" }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
  });

  it("'servi' finds Services in en-US and Serviços in pt-BR, ignoring accents", async () => {
    const user = userEvent.setup();
    await renderApp();
    await openPalette(user);
    await user.keyboard("servi");
    expect(await screen.findByRole("option", { name: /Go to Services/ })).toBeInTheDocument();
    await user.keyboard("{Escape}");

    await act(async () => {
      await setLanguage("pt-BR");
    });
    await openPalette(user);
    await user.keyboard("servi");
    expect(await screen.findByRole("option", { name: /Ir para Serviços/ })).toBeInTheDocument();
    await user.clear(within(screen.getByRole("dialog")).getByRole("combobox"));
    await user.keyboard("servicos");
    expect(await screen.findByRole("option", { name: /Ir para Serviços/ })).toBeInTheDocument();
    // The other locale's words work too.
    await user.clear(within(screen.getByRole("dialog")).getByRole("combobox"));
    await user.keyboard("services");
    expect(await screen.findByRole("option", { name: /Ir para Serviços/ })).toBeInTheDocument();
  });

  it("Enter on Start all calls services.start with empty targets and shows the job", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    await openPalette(user);
    await user.keyboard("start all");
    const row = await screen.findByRole("option", { name: /Start all/ });
    // The equivalent CLI command is on the row (rendered from the template).
    expect(row).toHaveAttribute("data-forge-cli", "forge start all");
    expect(within(row).getByText("forge start all")).toBeInTheDocument();
    expect(within(row).getAllByText(/^(Ctrl|Shift|S)$/)).toHaveLength(3);
    await user.keyboard("{Enter}");
    await waitFor(() => {
      expect(h.fake.calls.some((c) => c.method === "services.start")).toBe(true);
    });
    expect(h.fake.calls.find((c) => c.method === "services.start")?.params).toEqual({});
    await waitFor(() => {
      const messages = toastStore.getSnapshot().map((item) => item.message);
      expect(messages).toContain("Start all…");
      expect(messages).toContain("Start all: done");
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("lists navigation, per-instance start/stop and open-site entries", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    await openPalette(user);
    for (const name of [
      "Dashboard",
      "Sites",
      "PHP",
      "Databases",
      "Mail",
      "Logs",
      "Catalog",
      "Terminal",
      "Settings",
      "Diagnostics",
    ]) {
      expect(screen.getByRole("option", { name: new RegExp(`Go to ${name}`) })).toBeInTheDocument();
    }
    expect(screen.getByRole("option", { name: /Stop mysql\/default/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /Start redis\/default/ })).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: /Open loja\.test in the browser/ }),
    ).toBeInTheDocument();

    await user.keyboard("start redis");
    await user.keyboard("{Enter}");
    await waitFor(() => {
      expect(
        h.fake.calls.some(
          (c) =>
            c.method === "services.start" &&
            JSON.stringify(c.params) === JSON.stringify({ targets: ["redis/default"] }),
        ),
      ).toBe(true);
    });
  });

  it("navigates with Enter on a section", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    await openPalette(user);
    await user.keyboard("go to mail");
    await user.keyboard("{Enter}");
    await waitFor(() => {
      expect(pathOf(h)).toBe("/mail");
    });
  });
});

describe("shortcuts in the app", () => {
  it("Ctrl+1..9, Ctrl+L and Ctrl+, navigate", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    await user.keyboard("{Control>}3{/Control}");
    await waitFor(() => {
      expect(pathOf(h)).toBe("/sites");
    });
    await user.keyboard("{Control>}l{/Control}");
    await waitFor(() => {
      expect(pathOf(h)).toBe("/logs");
    });
    await user.keyboard("{Control>},{/Control}");
    await waitFor(() => {
      expect(pathOf(h)).toBe("/settings");
    });
  });

  it("Ctrl+Shift+S starts all", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    await user.keyboard("{Control>}{Shift>}s{/Shift}{/Control}");
    await waitFor(() => {
      expect(h.fake.calls.some((c) => c.method === "services.start")).toBe(true);
    });
  });

  it("an override persists and the old binding stops working; conflicts are rejected", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    const clash = await shortcutRegistry.set("nav.dashboard", "Ctrl+2");
    expect(clash.ok).toBe(false);
    expect((await shortcutRegistry.set("nav.sites", "Ctrl+Shift+3")).ok).toBe(true);
    expect(globalThis.localStorage.getItem("forgebox.shortcuts")).toContain("Ctrl+Shift+3");

    await user.keyboard("{Control>}3{/Control}");
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(pathOf(h)).toBe("/dashboard");
    await user.keyboard("{Control>}{Shift>}3{/Shift}{/Control}");
    await waitFor(() => {
      expect(pathOf(h)).toBe("/sites");
    });
  });

  it("does not fire inside a text field, except Ctrl+K", async () => {
    const user = userEvent.setup();
    const h = await renderApp();
    const field = document.createElement("input");
    field.type = "text";
    document.body.append(field);
    field.focus();
    await user.keyboard("{Control>}3{/Control}");
    await user.keyboard("{Control>}n{/Control}");
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(pathOf(h)).toBe("/dashboard");
    await user.keyboard("{Control>}k{/Control}");
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    field.remove();
  });

  it("ignores AltGr combinations (Ctrl+Alt on pt-BR keyboards)", async () => {
    const h = await renderApp();
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "3",
          code: "Digit3",
          ctrlKey: true,
          altKey: true,
          modifierAltGraph: true,
          bubbles: true,
        }),
      );
    });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(pathOf(h)).toBe("/dashboard");
  });
});

describe("global terminal shortcut", () => {
  it("is not registered while terminal.open is not implemented", async () => {
    const invoke = vi.fn<GlobalInvoke>(() => Promise.resolve({ registered: [], failed: [] }));
    globalShortcutBackend.invoke = invoke;
    await renderApp();
    await waitFor(() => {
      expect(invoke).toHaveBeenCalled();
    });
    expect(invoke.mock.calls.every(([, args]) => !args.enabled && args.chords.length === 0)).toBe(
      true,
    );
  });

  it("registers Ctrl+` and Ctrl+Alt+T once implemented, and reports a failure without breaking", async () => {
    const invoke = vi.fn<GlobalInvoke>(() =>
      Promise.resolve({
        registered: ["Ctrl+`"],
        failed: [{ accelerator: "Ctrl+Alt+T", error: "already registered" }],
      }),
    );
    globalShortcutBackend.invoke = invoke;
    await renderApp({ extra: { notImplemented: [] } });
    await waitFor(() => {
      expect(invoke.mock.calls.some(([, args]) => args.enabled)).toBe(true);
    });
    const enabled = invoke.mock.calls.find(([, args]) => args.enabled);
    expect(enabled?.[1].chords).toEqual(["Ctrl+`", "Ctrl+Alt+T"]);
    await waitFor(() => {
      expect(
        toastStore
          .getSnapshot()
          .map((t) => t.message)
          .join("\n"),
      ).toContain("The global shortcut Ctrl+Alt+T could not be registered: already registered");
    });
    // Still alive.
    expect(screen.getByTestId("app-shell")).toBeInTheDocument();
  });

  it("a rejected command is reported, not thrown", async () => {
    globalShortcutBackend.invoke = vi.fn<GlobalInvoke>(() =>
      Promise.reject(new Error("no plugin")),
    );
    await renderApp({ extra: { notImplemented: [] } });
    await waitFor(() => {
      expect(
        toastStore
          .getSnapshot()
          .map((t) => t.message)
          .join("\n"),
      ).toContain("no plugin");
    });
    expect(screen.getByTestId("app-shell")).toBeInTheDocument();
  });
});
