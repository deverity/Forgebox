import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState, type ReactElement } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineAction, resetActionsForTest } from "../../actions/registry";
import { fakeJob, type FakeScenario } from "../../ipc/fakeBridge";
import { ForgeStateProvider } from "../../state";
import { startHarness, type Harness } from "../../state/testFixtures";
import { ActionButton } from "./ActionButton";
import { AsyncView } from "./AsyncView";
import { CliCommand } from "./CliCommand";
import { PathField } from "./PathField";
import { PortField } from "./PortField";

const native = vi.hoisted(() => ({
  copyText: vi.fn<(text: string) => Promise<boolean>>(),
  pickPath: vi.fn<(kind: string, start?: string) => Promise<string | null>>(),
}));
vi.mock("../../ipc/native", () => native);

let harness: Harness | null = null;
beforeEach(() => {
  native.copyText.mockResolvedValue(true);
  native.pickPath.mockResolvedValue(null);
});
afterEach(async () => {
  resetActionsForTest();
  await harness?.layer.dispose();
  harness = null;
  vi.clearAllMocks();
});

async function mount(ui: ReactElement, scenario: FakeScenario = {}) {
  harness = await startHarness({ scenario });
  const h = harness;
  render(<ForgeStateProvider layer={h.layer}>{ui}</ForgeStateProvider>);
  await act(async () => {
    await h.runtime.settled();
  });
  return h;
}

const startServices = () =>
  defineAction({
    id: "services.start",
    method: "services.start",
    toParams: (targets: string[]) => ({ targets }),
  });

describe("CliCommand", () => {
  it("shows the command in mono, sets data-forge-cli and copies it", async () => {
    const user = userEvent.setup();
    const { container } = render(<CliCommand command="forge start mysql" />);
    expect(container.querySelector("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge start mysql",
    );
    expect(screen.getByText("forge start mysql").tagName).toBe("CODE");
    await user.click(screen.getByRole("button", { name: "Copy command" }));
    expect(native.copyText).toHaveBeenCalledWith("forge start mysql");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("renders nothing without a command", () => {
    const { container } = render(<CliCommand command={null} />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("ActionButton", () => {
  it("shows the loading text in the same frame, then the success check", async () => {
    const action = startServices();
    await mount(
      <ActionButton action={action} input={["mysql"]} loadingLabel="Starting mysql">
        Start
      </ActionButton>,
      { handlers: { "services.start": () => fakeJob<never>({ stepMs: 30 }) } },
    );
    // One synchronous act around the click: no timer and no await before the check below.
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "Start" }));
    });
    expect(screen.getByRole("button", { name: "Starting mysql" })).toBeDisabled();
    expect(await screen.findByText("Done")).toBeInTheDocument();
  });

  it("explains a failed job in words with at least one action", async () => {
    const action = startServices();
    await mount(
      <ActionButton action={action} input={["mysql"]}>
        Start
      </ActionButton>,
      {
        handlers: {
          "services.start": () =>
            fakeJob({
              outcome: {
                failed: {
                  code: "port_in_use",
                  message: "port 3306 is in use",
                  details: { reason: "port_in_use", args: { port: 3306, owner: "mysqld.exe" } },
                },
              },
            }),
        },
      },
    );
    await userEvent.setup().click(screen.getByRole("button", { name: "Start" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/3306/);
    expect(alert.querySelectorAll("[data-error-action]").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("is disabled with the reason when the method is not implemented", async () => {
    const action = startServices();
    await mount(
      <ActionButton action={action} input={["mysql"]}>
        Start
      </ActionButton>,
      { notImplemented: ["services.start"] },
    );
    const button = screen.getByRole("button", { name: "Start" });
    expect(button).toBeDisabled();
    expect(button).toHaveAccessibleDescription("Not available yet in this version of Forgebox.");
  });

  it("is disabled with the reason for local_only on a remote context", async () => {
    const action = defineAction({
      id: "tools.add_local",
      method: "tools.add_local",
      toParams: (source: string) => ({ source }),
    });
    await mount(
      <ActionButton action={action} input="C:/tools/x.zip">
        Add
      </ActionButton>,
      { hello: { local: false } },
    );
    expect(screen.getByRole("button", { name: "Add" })).toHaveAccessibleDescription(
      "Only available on the daemon's own machine.",
    );
  });

  it("is disabled when the role is too low", async () => {
    const action = startServices();
    await mount(
      <ActionButton action={action} input={["mysql"]}>
        Start
      </ActionButton>,
      { hello: { role: "viewer" } },
    );
    expect(screen.getByRole("button", { name: "Start" })).toHaveAccessibleDescription(
      /Requires the operator role/,
    );
  });

  it("shows the CLI in the tooltip and in data-forge-cli", async () => {
    const user = userEvent.setup();
    const action = startServices();
    await mount(
      <ActionButton action={action} input={["mysql", "redis"]}>
        Start
      </ActionButton>,
    );
    const wrapper = document.querySelector("[data-action='services.start']");
    expect(wrapper).toHaveAttribute("data-forge-cli", "forge start mysql redis");
    await user.hover(screen.getByRole("button", { name: "Start" }));
    const code = await screen.findByText("forge start mysql redis", { selector: "code" });
    expect(code).toHaveAttribute("data-slot", "command");
  });
});

describe("AsyncView", () => {
  type ViewState = "loading" | "ready" | "empty" | "error" | "off";
  function View({ state }: { state: ViewState }) {
    return (
      <AsyncView
        height={160}
        loading={state === "loading"}
        empty={state === "empty"}
        disconnected={state === "off"}
        {...(state === "error" ? { error: "boom" } : {})}
        emptyTitle="No sites"
        onRetry={() => undefined}
      >
        <p>data</p>
      </AsyncView>
    );
  }

  it("keeps the same height in every state", () => {
    const heights = new Set<string>();
    for (const state of ["loading", "ready", "empty", "error", "off"] as const) {
      const { container, unmount } = render(<View state={state} />);
      const root = container.firstElementChild as HTMLElement;
      expect(root).toHaveAttribute("data-async-view", state === "off" ? "disconnected" : state);
      heights.add(root.style.height);
      unmount();
    }
    expect([...heights]).toEqual(["160px"]);
  });

  it("shows skeletons, the empty title, the error with retry and the disconnected hint", () => {
    const { rerender } = render(<View state="loading" />);
    expect(document.querySelectorAll("[data-skeleton]").length).toBeGreaterThan(0);
    rerender(<View state="empty" />);
    expect(screen.getByText("No sites")).toBeInTheDocument();
    rerender(<View state="error" />);
    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    rerender(<View state="off" />);
    expect(screen.getByText("No connection to the daemon")).toBeInTheDocument();
  });
});

describe("PathField", () => {
  function Controlled() {
    const [value, setValue] = useState("");
    return <PathField label="Folder" value={value} onChange={setValue} />;
  }

  it("offers Browse for a local daemon and writes the picked path", async () => {
    native.pickPath.mockResolvedValue("C:/www/loja");
    await mount(<Controlled />);
    await userEvent.setup().click(screen.getByRole("button", { name: "Browse…" }));
    await waitFor(() => {
      expect(screen.getByLabelText("Folder")).toHaveValue("C:/www/loja");
    });
  });

  it("hides the dialog when hello.local is false and explains why", async () => {
    await mount(<Controlled />, { hello: { local: false } });
    expect(screen.queryByRole("button", { name: "Browse…" })).toBeNull();
    expect(screen.getByText(/runs on another machine/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Folder"), { target: { value: "/srv/www" } });
    expect(screen.getByLabelText("Folder")).toHaveValue("/srv/www");
  });
});

describe("PortField", () => {
  function Controlled() {
    const [value, setValue] = useState("");
    return <PortField label="Port" value={value} onChange={setValue} />;
  }

  it("sends one request per 250 ms window and shows the holder within 300 ms", async () => {
    const h = await mount(<Controlled />, {
      handlers: {
        "net.port_check": ({ port }) => ({
          free: false,
          holder: { pid: 4242, exe: `mysqld.exe (${String(port)})` },
        }),
      },
    });
    const input = screen.getByLabelText("Port");
    for (const text of ["3", "33", "330", "3306"]) {
      fireEvent.change(input, { target: { value: text } });
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    const typedAt = performance.now();
    await screen.findByText("in use by mysqld.exe (3306) (pid 4242)", {}, { timeout: 300 });
    expect(performance.now() - typedAt).toBeLessThan(300);
    expect(h.count("net.port_check")).toBe(1);
  });

  it("says free, and does not ask the daemon about a value that is not a port", async () => {
    const h = await mount(<Controlled />, {
      handlers: { "net.port_check": () => ({ free: true }) },
    });
    const input = screen.getByLabelText("Port");
    fireEvent.change(input, { target: { value: "70000" } });
    expect(await screen.findByText("Use a port between 1 and 65535.")).toBeInTheDocument();
    expect(h.count("net.port_check")).toBe(0);
    fireEvent.change(input, { target: { value: "8080" } });
    expect(await screen.findByText("free", {}, { timeout: 400 })).toBeInTheDocument();
    expect(h.count("net.port_check")).toBe(1);
  });
});
