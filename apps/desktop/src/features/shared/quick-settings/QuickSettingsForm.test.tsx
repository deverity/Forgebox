// Acceptance of M2-E2-T13 (criteria 1 and 2): the 8 control kinds, inline validation, the CLI
// of a changed field, only-changed-keys submit and the port conflict shown before submit.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18next from "i18next";
import type { ReactElement } from "react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { fakeJob, type FakeScenario } from "../../../ipc/fakeBridge";
import type { QuickSettingDto, ServicePortSettingDto } from "../../../ipc/generated";
import { ForgeStateProvider } from "../../../state";
import { startHarness, type Harness } from "../../../state/testFixtures";
import { QuickSettingsForm } from "./QuickSettingsForm";

vi.mock("../../../ipc/native", () => ({
  copyText: () => Promise.resolve(true),
  pickPath: () => Promise.resolve(null),
}));

beforeAll(async () => {
  await i18next.loadNamespaces("services");
});

function must<T>(value: T | null | undefined): T {
  if (value === null || value === undefined) throw new Error("expected a value");
  return value;
}

let harness: Harness | null = null;
afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

async function mount(ui: ReactElement, scenario: FakeScenario = {}) {
  harness = await startHarness({ scenario: { notImplemented: [], ...scenario } });
  const h = harness;
  render(<ForgeStateProvider layer={h.layer}>{ui}</ForgeStateProvider>);
  await act(async () => {
    await h.runtime.settled();
  });
  return h;
}

const ALL_KINDS: QuickSettingDto[] = [
  { key: "toggle_k", label: "Toggle K", kind: "toggle", value: false, apply: "none" },
  {
    key: "select_k",
    label: "Select K",
    kind: "select",
    options: ["a", "b"],
    value: "a",
    apply: "none",
  },
  {
    key: "number_k",
    label: "Number K",
    kind: "number",
    min: 10,
    max: 100,
    value: 50,
    default: 50,
    apply: "reload",
  },
  { key: "text_k", label: "Text K", kind: "text", value: "hello", apply: "none" },
  { key: "path_k", label: "Path K", kind: "path", value: "C:/tmp", apply: "restart" },
  { key: "port_k", label: "Port K", kind: "port", value: 9003, apply: "restart" },
  {
    key: "slider_k",
    label: "Slider K",
    kind: "slider",
    min: 0,
    max: 10,
    step: 1,
    value: 4,
    apply: "none",
  },
  {
    key: "multi_k",
    label: "Multi K",
    kind: "multi",
    options: ["x", "y"],
    value: ["x"],
    apply: "none",
  },
];

const SAVE = '[data-action="services.set_settings"]';

function save(): HTMLElement {
  const el = document.querySelector<HTMLElement>(SAVE);
  if (el === null) throw new Error("no save button");
  return el;
}

describe("control kinds", () => {
  it("renders the right control for each of the 8 kinds", async () => {
    await mount(<QuickSettingsForm target="mysql/default" settings={ALL_KINDS} />);
    const kinds = [...document.querySelectorAll("[data-kind]")].map((el) =>
      el.getAttribute("data-kind"),
    );
    expect(kinds).toEqual([
      "toggle",
      "select",
      "number",
      "text",
      "path",
      "port",
      "slider",
      "multi",
    ]);
    expect(screen.getByRole("switch", { name: "Toggle K" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Select K" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Number K" })).toHaveValue("50");
    expect(screen.getByRole("textbox", { name: "Text K" })).toHaveValue("hello");
    expect(screen.getByRole("textbox", { name: "Path K" })).toHaveValue("C:/tmp");
    expect(screen.getByRole("textbox", { name: "Port K" })).toHaveValue("9003");
    expect(screen.getByRole("slider", { name: "Slider K" })).toBeInTheDocument();
    const multi = screen.getByRole("group", { name: "Multi K" });
    expect(within(multi).getByRole("checkbox", { name: "x" })).toBeChecked();
    expect(within(multi).getByRole("checkbox", { name: "y" })).not.toBeChecked();
  });

  it("starts disabled with the reason, and an invalid value keeps it disabled", async () => {
    await mount(<QuickSettingsForm target="mysql/default" settings={ALL_KINDS} />);
    expect(save()).toHaveAttribute("data-disabled-reason", "Nothing changed yet.");
    const number = screen.getByRole("textbox", { name: "Number K" });
    fireEvent.change(number, { target: { value: "5" } });
    expect(screen.getByText("Use a value between 10 and 100.")).toBeInTheDocument();
    expect(save()).toHaveAttribute("data-disabled-reason", "Fix the highlighted fields first.");
    fireEvent.change(number, { target: { value: "abc" } });
    expect(screen.getByText("Type a number.")).toBeInTheDocument();
    fireEvent.change(number, { target: { value: "60" } });
    expect(save()).not.toHaveAttribute("data-disabled-reason");
  });

  it("toggles, checks and undoes per field", async () => {
    const user = userEvent.setup();
    await mount(<QuickSettingsForm target="mysql/default" settings={ALL_KINDS} />);
    await user.click(screen.getByRole("switch", { name: "Toggle K" }));
    expect(screen.getByRole("switch", { name: "Toggle K" })).toBeChecked();
    const row = document.querySelector('[data-setting="toggle_k"]');
    expect(row).toHaveAttribute("data-dirty", "true");
    await user.click(within(row as HTMLElement).getByRole("button", { name: /Undo/ }));
    expect(screen.getByRole("switch", { name: "Toggle K" })).not.toBeChecked();
    expect(row).not.toHaveAttribute("data-dirty");
    // A field that is not dirty but differs from its default offers "Use default".
    fireEvent.change(screen.getByRole("textbox", { name: "Number K" }), {
      target: { value: "70" },
    });
    await user.click(
      within(must(document.querySelector<HTMLElement>('[data-setting="number_k"]'))).getByRole(
        "button",
        {
          name: /Undo/,
        },
      ),
    );
    expect(screen.getByRole("textbox", { name: "Number K" })).toHaveValue("50");
  });
});

describe("submit", () => {
  const php: QuickSettingDto[] = [
    {
      key: "memory_limit",
      label: "Memory limit",
      kind: "text",
      value: "128M",
      default: "128M",
      apply: "reload",
    },
    {
      key: "display_errors",
      label: "Display errors",
      kind: "toggle",
      value: true,
      apply: "reload",
    },
  ];

  it("shows the CLI of the changed field and says it reloads", async () => {
    await mount(<QuickSettingsForm target="php@8.4" settings={php} />);
    fireEvent.change(screen.getByRole("textbox", { name: "Memory limit" }), {
      target: { value: "1G" },
    });
    expect(save()).toHaveAttribute("data-forge-cli", "forge service set php@8.4 memory_limit=1G");
    expect(screen.getByTestId("apply-note")).toHaveTextContent(
      "Applying these changes reloads the service.",
    );
  });

  it("sends only the changed keys through services.set", async () => {
    const h = await mount(<QuickSettingsForm target="php@8.4" settings={php} />, {
      handlers: {
        "services.set": () => fakeJob({ outcome: { done: { instances: [], pools: [] } } }),
      },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Memory limit" }), {
      target: { value: "1G" },
    });
    const user = userEvent.setup();
    await user.click(within(save()).getByRole("button", { name: "Apply changes" }));
    await waitFor(() => {
      expect(h.count("services.set")).toBe(1);
    });
    const call = h.fake.calls.find((c) => c.method === "services.set");
    expect(call?.params).toEqual({ target: "php@8.4", settings: { memory_limit: "1G" } });
  });
});

describe("ports", () => {
  const ports: ServicePortSettingDto[] = [
    { name: "main", port: 3306, default: 3306, protocol: "tcp" },
  ];
  const handlers = {
    "net.port_check": ({ port }: { port: number }) =>
      port === 6379 ? { free: false, reserved_by: "redis/default" } : { free: true },
  };

  it("shows a port reserved by another owner before submit and blocks it", async () => {
    await mount(<QuickSettingsForm target="mysql/default" settings={[]} ports={ports} />, {
      handlers,
    });
    const input = screen.getByRole("textbox", { name: "ports.main" });
    expect(input).toHaveValue("3306");
    fireEvent.change(input, { target: { value: "6379" } });
    // The message shows under the field and, as the reason, on the disabled button.
    expect(await screen.findAllByText("Port already in use by redis/default.")).toHaveLength(2);
    expect(document.querySelector('[data-setting="ports.main"]')).toHaveAttribute(
      "data-conflict",
      "true",
    );
    expect(save()).toHaveAttribute("data-disabled-reason", "Port already in use by redis/default.");
    // A free port clears the conflict and a port change always restarts.
    fireEvent.change(input, { target: { value: "3307" } });
    await waitFor(() => {
      expect(save()).not.toHaveAttribute("data-disabled-reason");
    });
    expect(save()).toHaveAttribute(
      "data-forge-cli",
      "forge service set mysql/default ports.main=3307",
    );
    expect(screen.getByTestId("apply-note")).toHaveAttribute("data-apply", "restart");
  });

  it("does not call the saved port a conflict", async () => {
    await mount(<QuickSettingsForm target="mysql/default" settings={[]} ports={ports} />, {
      handlers: { "net.port_check": () => ({ free: false, reserved_by: "mysql/default" }) },
    });
    expect(screen.queryByText(/already in use/)).toBeNull();
    expect(save()).toHaveAttribute("data-disabled-reason", "Nothing changed yet.");
  });
});
