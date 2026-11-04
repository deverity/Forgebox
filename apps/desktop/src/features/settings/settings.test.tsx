import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QA_INVALID_EDITOR, settingsHandlers } from "../../../qa/fixtures/scenarios/settings";
import { renderApp, type AppHarness } from "../../app/testApp";
import { DEFAULT_SHORTCUTS, shortcutRegistry } from "../../app/shortcuts/registry";
import { fakeError } from "../../ipc/fakeBridge";
import type { GuiPrefsApi } from "./guiPrefs";

const prefs = vi.hoisted(() => ({
  get: vi.fn(() => Promise.resolve({ theme: "system", close_to_tray: true })),
  set: vi.fn(() => Promise.resolve()),
}));
vi.mock("./guiPrefs", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./guiPrefs")>()),
  defaultGuiPrefs: (): GuiPrefsApi => prefs as unknown as GuiPrefsApi,
}));

const native = vi.hoisted(() => ({
  revealPath: vi.fn(() => Promise.resolve(true)),
  copyText: vi.fn(() => Promise.resolve(true)),
}));
vi.mock("../../ipc/native", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc/native")>()),
  ...native,
}));

let harness: AppHarness | null = null;

beforeEach(() => {
  localStorage.clear();
});

afterEach(async () => {
  prefs.get.mockClear();
  prefs.set.mockClear();
  native.revealPath.mockClear();
  for (const def of DEFAULT_SHORTCUTS) await shortcutRegistry.reset(def.id);
  await harness?.layer.dispose();
  harness = null;
});

type Options = Parameters<typeof renderApp>[0];

async function open(options: Options & { fake?: ReturnType<typeof settingsHandlers> } = {}) {
  const fake = options.fake ?? settingsHandlers();
  harness = await renderApp({
    path: "/settings",
    ...options,
    extra: { ...options.extra, handlers: { ...fake.handlers, ...options.extra?.handlers } },
  });
  return { h: harness, config: fake, user: userEvent.setup() };
}

const calls = (h: AppHarness, method: string) => h.fake.calls.filter((c) => c.method === method);

async function gotoEditor(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByTestId("settings-nav-editor"));
  return screen.findByTestId("setting-editor.command");
}

describe("Settings layout", () => {
  it("opens on Geral with the read-only root and a sub-navigation", async () => {
    await open();
    expect(await screen.findByRole("heading", { level: 1, name: "Settings" })).toBeInTheDocument();
    expect(await screen.findByTestId("root-path")).toHaveTextContent("C:/Forgebox");
    for (const id of ["general", "editor", "shortcuts", "advanced"]) {
      expect(screen.getByTestId(`settings-nav-${id}`)).toBeInTheDocument();
    }
    expect(screen.getByTestId("settings-nav-general")).toHaveAttribute("aria-current", "page");
  });

  it("filters sections and rows by label", async () => {
    const { user } = await open();
    const search = await screen.findByRole("searchbox", { name: "Search settings" });
    await user.type(search, "templat");
    expect(screen.queryByTestId("settings-nav-general")).toBeNull();
    expect(screen.getByTestId("settings-nav-advanced")).toHaveAttribute("aria-current", "page");
    expect(screen.getByTestId("setting-templates")).toBeInTheDocument();
    expect(screen.queryByTestId("setting-logs")).toBeNull();
    await user.clear(search);
    await user.type(search, "zzzz");
    expect(await screen.findByTestId("settings-no-match")).toBeInTheDocument();
  });

  it("finds a schema row by its label from another section", async () => {
    const { user } = await open();
    await user.type(await screen.findByRole("searchbox"), "integrated terminal sessions");
    await waitFor(() => {
      expect(screen.getByTestId("settings-nav-editor")).toHaveAttribute("aria-current", "page");
    });
    expect(screen.getByTestId("setting-terminal.max_sessions")).toBeInTheDocument();
    expect(screen.queryByTestId("setting-editor.command")).toBeNull();
  });

  it("explains a failed config_list with a retry", async () => {
    await open({
      extra: {
        handlers: {
          "system.config_list": () => {
            fakeError("timeout", "daemon did not answer");
          },
        },
      },
    });
    expect(
      await screen.findByTestId("settings-load-error", {}, { timeout: 4000 }),
    ).toBeInTheDocument();
  });
});

describe("Editor and terminal rows (generated from the schema)", () => {
  it("builds the right control for each key", async () => {
    const { user } = await open();
    await gotoEditor(user);
    expect(screen.getByTestId("setting-editor.command")).toHaveAttribute("data-control", "text");
    expect(screen.getByTestId("setting-terminal.command")).toHaveAttribute("data-control", "text");
    expect(screen.getByTestId("setting-terminal.integrated_shell")).toHaveAttribute(
      "data-control",
      "select",
    );
    expect(screen.getByTestId("setting-terminal.max_sessions")).toHaveAttribute(
      "data-control",
      "number",
    );
    expect(screen.getByRole("textbox", { name: "Editor command" })).toHaveValue("");
    expect(screen.getByRole("textbox", { name: "Integrated terminal sessions" })).toHaveValue("16");
  });

  it("saves editor.command through config_set and shows the forge command", async () => {
    const { h, config, user } = await open();
    const row = await gotoEditor(user);
    await user.click(within(row).getByRole("textbox", { name: "Editor command" }));
    await user.paste("code -g {path}:{line}");
    expect(within(row).getByText("forge config set editor.command", { exact: false })).toBeTruthy();
    await user.click(within(row).getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(calls(h, "system.config_set")).toHaveLength(1);
    });
    expect(calls(h, "system.config_set")[0]?.params).toEqual({
      key: "editor.command",
      value: "code -g {path}:{line}",
    });
    expect(await within(row).findByTestId("setting-saved")).toBeInTheDocument();
    expect(config.values().editor.command).toBe("code -g {path}:{line}");
    const cli = row.querySelector("[data-forge-cli]");
    expect(cli?.getAttribute("data-forge-cli")).toBe(
      'forge config set editor.command "code -g {path}:{line}"',
    );
    // The list refetch brings the saved value back and the row is clean again.
    await waitFor(() => {
      expect(within(row).getByRole("textbox", { name: "Editor command" })).toHaveValue(
        "code -g {path}:{line}",
      );
    });
    expect(within(row).getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("fills a preset without saving it", async () => {
    const { h, user } = await open();
    const row = await gotoEditor(user);
    await user.click(screen.getByTestId("preset-editor.command-phpstorm"));
    expect(within(row).getByRole("textbox", { name: "Editor command" })).toHaveValue(
      "phpstorm --line {line} {path}",
    );
    expect(calls(h, "system.config_set")).toHaveLength(0);
  });

  it("shows the cause of config_invalid inline and goes back to the old value", async () => {
    const { h, user } = await open();
    const row = await gotoEditor(user);
    const input = within(row).getByRole("textbox", { name: "Editor command" });
    await user.type(input, QA_INVALID_EDITOR);
    await user.click(within(row).getByRole("button", { name: "Save" }));
    const error = await within(row).findByTestId("setting-error");
    expect(error).toHaveTextContent("The configuration is not valid.");
    expect(within(error).getByTestId("setting-error-cause")).toHaveTextContent(
      "editor.command: unbalanced quotes",
    );
    expect(calls(h, "system.config_set")).toHaveLength(1);
    expect(within(row).getByRole("textbox", { name: "Editor command" })).toHaveValue("");
    expect(within(row).queryByTestId("setting-saved")).toBeNull();
  });

  it("refuses a number outside the schema range before calling the daemon", async () => {
    const { h, user } = await open();
    await gotoEditor(user);
    const row = screen.getByTestId("setting-terminal.max_sessions");
    const input = within(row).getByRole("textbox", { name: "Integrated terminal sessions" });
    await user.clear(input);
    await user.type(input, "999");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    expect(await within(row).findByText("Use a value between 1 and 256.")).toBeInTheDocument();
    expect(calls(h, "system.config_set")).toHaveLength(0);
  });

  it("saves a select on change, sending null for 'Automatic'", async () => {
    const fake = settingsHandlers({
      terminal: { command: null, integrated_shell: "cmd", max_sessions: 16 },
    });
    const { h, user } = await open({ fake });
    await gotoEditor(user);
    const row = screen.getByTestId("setting-terminal.integrated_shell");
    await user.click(within(row).getByRole("combobox", { name: "Integrated terminal shell" }));
    await user.click(await screen.findByRole("option", { name: "Automatic" }));
    await waitFor(() => {
      expect(calls(h, "system.config_set")).toHaveLength(1);
    });
    expect(calls(h, "system.config_set")[0]?.params).toEqual({
      key: "terminal.integrated_shell",
      value: null,
    });
  });
});

describe("requires and live updates", () => {
  it("offers the daemon.restart follow-up inline", async () => {
    const { h, user } = await open({
      extra: {
        handlers: {
          "system.config_set": (params) => ({
            key: params.key,
            value: params.value,
            is_default: false,
            requires: ["daemon.restart"],
          }),
          "daemon.restart": () => ({ ok: true, message: null }),
        },
      },
    });
    const row = await gotoEditor(user);
    await user.type(within(row).getByRole("textbox", { name: "Editor command" }), "notepad");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    const requires = await within(row).findByTestId("setting-requires");
    expect(within(requires).getByText(/after the daemon restarts/)).toBeInTheDocument();
    await user.click(within(requires).getByRole("button", { name: "Restart the daemon" }));
    await waitFor(() => {
      expect(calls(h, "daemon.restart")).toHaveLength(1);
    });
  });

  it("asks before rotating the CA when a TLD change requires it", async () => {
    const { h, user } = await open({
      extra: {
        handlers: {
          "system.config_set": (params) => ({
            key: params.key,
            value: params.value,
            is_default: false,
            requires: ["tls.ca_rotate"],
          }),
        },
      },
    });
    const row = await gotoEditor(user);
    await user.type(within(row).getByRole("textbox", { name: "Editor command" }), "x");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    const requires = await within(row).findByTestId("setting-requires");
    await user.click(within(requires).getByRole("button", { name: "Renew the CA" }));
    expect(within(requires).getByText(/re-issues every certificate/)).toBeInTheDocument();
    expect(calls(h, "tls.ca_rotate")).toHaveLength(0);
  });

  it("updates a row when another client changes the file (config.changed)", async () => {
    const { h, config, user } = await open();
    const row = await gotoEditor(user);
    expect(within(row).getByRole("textbox", { name: "Editor command" })).toHaveValue("");
    await act(async () => {
      config.values().editor.command = "notepad {path}";
      h.fake.publish("config.changed", { v: 1, keys: ["editor.command"], requires: [] });
      await h.settle();
    });
    await waitFor(() => {
      expect(within(row).getByRole("textbox", { name: "Editor command" })).toHaveValue(
        "notepad {path}",
      );
    });
    expect(calls(h, "system.config_set")).toHaveLength(0);
  });
});

describe("Geral: GUI-local preferences never reach config_set", () => {
  it("saves theme, close to tray and language only in the GUI prefs", async () => {
    const { h, user } = await open();
    await screen.findByTestId("root-path");
    await user.click(await screen.findByRole("radio", { name: "Dark" }));
    await user.click(screen.getByRole("switch", { name: "Close to tray" }));
    await waitFor(() => {
      expect(prefs.set).toHaveBeenCalledWith({ theme: "dark" });
      expect(prefs.set).toHaveBeenCalledWith({ close_to_tray: false });
    });
    expect(calls(h, "system.config_set")).toHaveLength(0);
    expect(calls(h, "system.config_get")).toHaveLength(0);
  });

  it("goes back to the old value and says so when the pref cannot be saved", async () => {
    prefs.set.mockImplementationOnce(() => Promise.reject(new Error("disk full")));
    const { user } = await open();
    const toggle = await screen.findByRole("switch", { name: "Close to tray" });
    await user.click(toggle);
    expect(await screen.findByTestId("prefs-error")).toHaveTextContent("disk full");
    await waitFor(() => {
      expect(screen.getByRole("switch", { name: "Close to tray" })).toBeChecked();
    });
  });

  it("opens the root folder only on a local daemon", async () => {
    const { user } = await open();
    await user.click(await screen.findByRole("button", { name: "Open" }));
    expect(native.revealPath).toHaveBeenCalledWith("C:/Forgebox");
  });

  it("hides 'Open' for the root on a remote daemon", async () => {
    await open({ scenario: "remote" });
    await screen.findByTestId("root-path");
    expect(screen.queryByRole("button", { name: "Open" })).toBeNull();
  });

  it("goes to the onboarding when asked to run it again", async () => {
    const { h, user } = await open();
    await user.click(await screen.findByRole("button", { name: "Run onboarding again" }));
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/onboarding");
    });
  });
});

describe("Testar", () => {
  it("runs the saved editor command on the root, only when nothing is unsaved", async () => {
    const fake = settingsHandlers({ editor: { command: "code -g {path}:{line}" } });
    const { h, user } = await open({ fake });
    const row = await gotoEditor(user);
    const test = within(row).getByTestId("test-editor");
    await user.type(within(row).getByRole("textbox", { name: "Editor command" }), " --reuse");
    expect(test).toBeDisabled();
    await user.click(within(row).getByRole("button", { name: "Discard" }));
    await user.click(test);
    await waitFor(() => {
      expect(calls(h, "shell.open_editor")).toHaveLength(1);
    });
    expect(calls(h, "shell.open_editor")[0]?.params).toEqual({ path: "C:/Forgebox" });
    expect(await within(row).findByTestId("test-editor-ok")).toBeInTheDocument();
  });

  it("opens a shell in the root to test the terminal command", async () => {
    const { h, user } = await open();
    await gotoEditor(user);
    await user.click(screen.getByTestId("test-terminal"));
    await waitFor(() => {
      expect(calls(h, "shell.open")).toHaveLength(1);
    });
    expect(calls(h, "shell.open")[0]?.params).toEqual({ dir: "C:/Forgebox" });
  });

  it("shows the daemon's refusal when the command cannot start", async () => {
    const { user } = await open({
      extra: {
        handlers: {
          "shell.open_editor": () => {
            fakeError("not_found", "editor not found: code");
          },
        },
      },
    });
    await gotoEditor(user);
    await user.click(screen.getByTestId("test-editor"));
    const row = screen.getByTestId("setting-editor.command");
    const notice = await within(row).findByRole("alert");
    expect(notice).toHaveAttribute("data-error-code", "not_found");
  });

  it("is not offered on a remote daemon", async () => {
    const { user } = await open({ scenario: "remote" });
    await gotoEditor(user);
    expect(screen.queryByTestId("test-editor")).toBeNull();
    expect(screen.queryByTestId("test-terminal")).toBeNull();
    expect(screen.getByTestId("setting-editor.command")).toBeInTheDocument();
  });
});

describe("Atalhos", () => {
  const press = (id: string, init: KeyboardEventInit & { code: string }) => {
    fireEvent.keyDown(screen.getByTestId(`shortcut-change-${id}`), init);
  };

  async function gotoShortcuts(user: ReturnType<typeof userEvent.setup>) {
    await user.click(await screen.findByTestId("settings-nav-shortcuts"));
    await screen.findByTestId("shortcut-sites.new");
  }

  it("rebinds a command and persists the override", async () => {
    const { user } = await open();
    await gotoShortcuts(user);
    await user.click(screen.getByTestId("shortcut-change-sites.new"));
    press("sites.new", { key: "Y", code: "KeyY", ctrlKey: true, shiftKey: true });
    await waitFor(() => {
      expect(shortcutRegistry.chordOf("sites.new")).toBe("Ctrl+Shift+Y");
    });
    expect(JSON.parse(localStorage.getItem("forgebox.shortcuts") ?? "{}")).toEqual({
      "sites.new": "Ctrl+Shift+Y",
    });
    expect(await screen.findByTestId("shortcut-reset-sites.new")).toBeInTheDocument();
  });

  it("rejects a chord another command owns, naming it, and keeps the old one", async () => {
    const { user } = await open();
    await gotoShortcuts(user);
    await user.click(screen.getByTestId("shortcut-change-sites.new"));
    press("sites.new", { key: "l", code: "KeyL", ctrlKey: true });
    const alert = await within(screen.getByTestId("shortcut-sites.new")).findByTestId(
      "shortcut-error",
    );
    expect(alert).toHaveTextContent("Ctrl+L is already used by “Open logs”");
    expect(shortcutRegistry.chordOf("sites.new")).toBe("Ctrl+N");
    expect(localStorage.getItem("forgebox.shortcuts")).toBeNull();
  });

  it("resets one binding and all of them", async () => {
    const { user } = await open();
    await gotoShortcuts(user);
    await user.click(screen.getByTestId("shortcut-change-sites.new"));
    press("sites.new", { key: "Y", code: "KeyY", ctrlKey: true });
    await user.click(await screen.findByTestId("shortcut-reset-sites.new"));
    await waitFor(() => {
      expect(shortcutRegistry.chordOf("sites.new")).toBe("Ctrl+N");
    });
    await user.click(screen.getByTestId("shortcut-change-logs.open"));
    press("logs.open", { key: "Y", code: "KeyY", ctrlKey: true });
    await user.click(await screen.findByTestId("shortcuts-reset-all"));
    await waitFor(() => {
      expect(shortcutRegistry.chordOf("logs.open")).toBe("Ctrl+L");
    });
  });

  it("does not fire the command while recording and cancels with Escape", async () => {
    const { h, user } = await open();
    await gotoShortcuts(user);
    await user.click(screen.getByTestId("shortcut-change-sites.new"));
    press("sites.new", { key: "n", code: "KeyN", ctrlKey: true });
    // Ctrl+N would have navigated to /sites with the new-site sheet; it was captured instead.
    expect(h.router.state.location.pathname).toBe("/settings");
    await user.click(screen.getByTestId("shortcut-change-sites.new"));
    press("sites.new", { key: "Escape", code: "Escape" });
    expect(shortcutRegistry.chordOf("sites.new")).toBe("Ctrl+N");
  });
});

describe("Avançado", () => {
  it("shows the templates folder, the logs link and the diagnostics link", async () => {
    const { h, user } = await open();
    await user.click(await screen.findByTestId("settings-nav-advanced"));
    expect(await screen.findByTestId("templates-path")).toHaveTextContent(
      "C:/Forgebox/etc/templates",
    );
    await user.click(screen.getByRole("button", { name: "Copy path" }));
    expect(native.copyText).toHaveBeenCalledWith("C:/Forgebox/etc/templates");
    await user.click(screen.getByRole("link", { name: "Open diagnostics" }));
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/diagnostics");
    });
  });
});

describe("Ctrl+,", () => {
  it("opens the screen from anywhere", async () => {
    harness = await renderApp({ path: "/sites" });
    await screen.findByTestId("page-sites");
    fireEvent.keyDown(window, { key: ",", code: "Comma", ctrlKey: true });
    await waitFor(() => {
      expect(harness?.router.state.location.pathname).toBe("/settings");
    });
  });
});
