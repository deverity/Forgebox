// Acceptance of M2-E1-T18 (webview side): inactive in the 0.2.0 build, changelog before apply,
// explicit confirmation, autostart disabled with a reason in portable mode, footer state.
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { renderApp } from "../../app/testApp";
import {
  INACTIVE_SNAPSHOT,
  inertUpdaterApi,
  type AutostartStatus,
  type UpdaterApi,
  type UpdaterSnapshot,
} from "./api";
import { createUpdaterStore, setUpdaterStoreForTests } from "./store";
import { AutostartToggle, UpdaterPanel } from "./UpdaterPanel";

const AVAILABLE: UpdaterSnapshot = {
  active: true,
  current_version: "0.2.0",
  phase: "available",
  available: { version: "0.3.0", notes: "- faster startup\n- fixes", date: null },
  last_checked_ms: 1,
  error: null,
};

function fakeApi(snapshot: UpdaterSnapshot, autostart?: AutostartStatus) {
  let push: (s: UpdaterSnapshot) => void = () => undefined;
  const spies = {
    status: vi.fn(() => Promise.resolve(snapshot)),
    check: vi.fn(() => Promise.resolve(snapshot)),
    apply: vi.fn(() => Promise.resolve()),
    setChannel: vi.fn<UpdaterApi["setChannel"]>(() => Promise.resolve()),
    setAutostart: vi.fn((enabled: boolean) =>
      Promise.resolve({ available: true, enabled, reason: null }),
    ),
  };
  const api: UpdaterApi = {
    ...inertUpdaterApi,
    ...spies,
    subscribe: (onChange) => {
      push = onChange;
      return Promise.resolve(() => undefined);
    },
    autostart: () =>
      Promise.resolve(autostart ?? { available: true, enabled: false, reason: null }),
  };
  return {
    api,
    spies,
    push: (s: UpdaterSnapshot) => {
      push(s);
    },
  };
}

beforeAll(async () => {
  // Feature namespaces load on demand (the Settings route does this before rendering).
  await i18n.loadNamespaces("updater");
});

afterEach(() => {
  setUpdaterStoreForTests(null);
});

describe("inactive updater (0.2.0)", () => {
  it("says that app updates arrive in 1.0 and offers no actions", async () => {
    setUpdaterStoreForTests(createUpdaterStore(inertUpdaterApi));
    render(<UpdaterPanel />);
    expect(await screen.findByTestId("updater-inactive")).toHaveTextContent(
      "App updates arrive in 1.0.",
    );
    expect(screen.queryByRole("button", { name: "Check for updates" })).toBeNull();
  });

  it("keeps the footer notice in the shell", async () => {
    setUpdaterStoreForTests(createUpdaterStore(inertUpdaterApi));
    await renderApp();
    expect(screen.getByTestId("sidebar-version")).toHaveTextContent(
      /^v\S+ · App updates arrive in 1\.0$/,
    );
  });
});

describe("active updater", () => {
  it("shows the changelog and applies only after the user confirms", async () => {
    const { api, spies } = fakeApi(AVAILABLE);
    setUpdaterStoreForTests(createUpdaterStore(api));
    render(<UpdaterPanel />);

    expect(await screen.findByTestId("updater-state")).toHaveTextContent(
      "Version 0.3.0 is available",
    );
    expect(screen.getByTestId("updater-changelog")).toHaveTextContent("faster startup");

    await userEvent.click(screen.getByRole("button", { name: "Update and restart" }));
    expect(spies.apply).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog")).toHaveTextContent("stop the daemon");

    await userEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(spies.apply).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: "Update and restart" }));
    await userEvent.click(screen.getByRole("button", { name: "Update now" }));
    expect(spies.apply).toHaveBeenCalledOnce();
  });

  it("follows state events, shows a localized failure and saves the channel", async () => {
    const { api, spies, push } = fakeApi({ ...AVAILABLE, phase: "idle", available: null });
    setUpdaterStoreForTests(createUpdaterStore(api));
    render(<UpdaterPanel />);
    await screen.findByTestId("updater-state");

    act(() => {
      push({
        ...AVAILABLE,
        phase: "error",
        error: { code: "verification_failed", reason: "updater_bad_signature", message: "Bad sig" },
      });
    });
    expect(await screen.findByTestId("updater-error")).toHaveTextContent("Bad sig");
    expect(screen.getByTestId("updater-state")).toHaveTextContent("The update failed");

    await userEvent.click(screen.getByRole("radio", { name: "Beta" }));
    expect(spies.setChannel).toHaveBeenCalledWith("beta");
  });

  it("announces the update in the sidebar footer", async () => {
    const { api } = fakeApi(AVAILABLE);
    setUpdaterStoreForTests(createUpdaterStore(api));
    await renderApp();
    await waitFor(() => {
      expect(screen.getByTestId("sidebar-version")).toHaveTextContent("Update 0.3.0 available");
    });
    expect(screen.getByTestId("sidebar-version")).toHaveAttribute("data-update", "available");
  });
});

describe("autostart toggle", () => {
  it("enables the entry through the API", async () => {
    const { api, spies } = fakeApi(INACTIVE_SNAPSHOT);
    setUpdaterStoreForTests(createUpdaterStore(api));
    render(<AutostartToggle />);
    const toggle = await screen.findByRole("switch", { name: "Start with Windows (minimized)" });
    expect(toggle).not.toBeDisabled();
    await userEvent.click(toggle);
    expect(spies.setAutostart).toHaveBeenCalledWith(true);
    await waitFor(() => {
      expect(screen.getByRole("switch")).toBeChecked();
    });
  });

  it("is disabled with the reason in portable mode", async () => {
    const reason = "Autostart is disabled in portable mode.";
    const { api } = fakeApi(INACTIVE_SNAPSHOT, { available: false, enabled: false, reason });
    setUpdaterStoreForTests(createUpdaterStore(api));
    render(<AutostartToggle />);
    expect(await screen.findByRole("switch")).toBeDisabled();
    expect(screen.getByTestId("autostart-reason")).toHaveTextContent(reason);
  });
});
