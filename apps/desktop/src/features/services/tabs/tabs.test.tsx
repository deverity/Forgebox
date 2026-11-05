// Acceptance of M2-E2-T13 (criteria 3, 4 and 5): Configuracao (editor locally, fs.read viewer
// with managed blocks when remote), Versoes (CLI strings, remove blocked while in use) and
// Avancado (services.update with only the changed fields).
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18next from "i18next";
import type { ReactElement } from "react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { fakeJob, type FakeScenario } from "../../../ipc/fakeBridge";
import type {
  InstanceAdvancedDto,
  ServiceSettingsDto,
  ToolInfoDto,
  ToolVersionDto,
  UpdateHistoryDto,
} from "../../../ipc/generated";
import { ForgeStateProvider } from "../../../state";
import { startHarness, type Harness } from "../../../state/testFixtures";
import { AdvancedTab } from "./AdvancedTab";
import { ConfigTab } from "./ConfigTab";
import { VersionsTab } from "./VersionsTab";
import { advancedPatch, draftOf } from "./advancedModel";
import { cliSpecFor, inUse, rollbackCandidates } from "./versionsModel";

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

function action(id: string, root: ParentNode = document): HTMLElement {
  const el = root.querySelector<HTMLElement>(`[data-action="${id}"]`);
  if (el === null) throw new Error(`no action ${id}`);
  return el;
}

const SETTINGS: ServiceSettingsDto = {
  target: "mysql/default",
  tool: "mysql",
  settings: [
    {
      key: "max_connections",
      label: "Max connections",
      kind: "number",
      value: 151,
      apply: "restart",
    },
  ],
  ports: [{ name: "main", port: 3306, default: 3306, protocol: "tcp" }],
  advanced: {
    autostart: false,
    args: [],
    env: {},
    restart_policy: { on: "failure", max: 3, window_secs: 60 },
    restart_policy_default: { on: "failure", max: 3, window_secs: 60 },
    versions: ["8.4.3", "8.4.2"],
  },
};

const INI =
  "[mysqld]\n# forgebox:begin quick\nmax_connections = 151\n# forgebox:end quick\nmine = 1\n";

const configHandlers = {
  "services.settings": () => SETTINGS,
  "services.config_files": () => ({
    files: [
      {
        role: "main",
        path: "C:/Forgebox/etc/mysql/my.ini",
        fs_path: "etc/mysql/my.ini",
        managed: true,
      },
    ],
  }),
  "services.config": () => ({ files: [] }),
  "fs.read": ({ path }: { path: string }) => ({
    path,
    encoding: "utf8" as const,
    content: INI,
    size: INI.length,
    truncated: false,
    modified: null,
    etag: "e",
  }),
};

describe("Configuracao", () => {
  it("opens the file in the editor when the daemon is local", async () => {
    const user = userEvent.setup();
    const h = await mount(<ConfigTab target="mysql/default" />, { handlers: configHandlers });
    expect(await screen.findByRole("textbox", { name: "Max connections" })).toHaveValue("151");
    expect(screen.getByRole("textbox", { name: "ports.main" })).toHaveValue("3306");
    expect(await screen.findByText("C:/Forgebox/etc/mysql/my.ini")).toBeInTheDocument();
    const open = action("services.open_config");
    expect(open).toHaveAttribute("data-forge-cli", "forge service config mysql/default");
    await user.click(within(open).getByRole("button", { name: "Open file" }));
    await waitFor(() => {
      expect(h.count("services.config")).toBe(1);
    });
    expect(screen.queryByTestId("file-viewer")).toBeNull();
  });

  it("becomes the read-only fs.read viewer with managed blocks highlighted when remote", async () => {
    const user = userEvent.setup();
    const h = await mount(<ConfigTab target="mysql/default" />, {
      handlers: configHandlers,
      hello: { local: false, role: "admin" },
    });
    await screen.findByText("C:/Forgebox/etc/mysql/my.ini");
    expect(document.querySelector('[data-action="services.open_config"]')).toBeNull();
    await user.click(screen.getByRole("button", { name: "Open file: main" }));
    const viewer = await screen.findByTestId("file-viewer");
    await waitFor(() => {
      expect(viewer.querySelectorAll("[data-managed]").length).toBe(3);
    });
    expect(h.fake.calls.find((c) => c.method === "fs.read")?.params).toEqual({
      path: "etc/mysql/my.ini",
    });
    const lines = [...viewer.querySelectorAll("pre > div")].map((el) => [
      el.textContent,
      el.hasAttribute("data-managed"),
    ]);
    expect(lines.slice(0, 5)).toEqual([
      ["[mysqld]", false],
      ["# forgebox:begin quick", true],
      ["max_connections = 151", true],
      ["# forgebox:end quick", true],
      ["mine = 1", false],
    ]);
    expect(viewer.querySelector("textarea, input")).toBeNull();
  });
});

function version(id: string, v: string, patch: Partial<ToolVersionDto> = {}): ToolVersionDto {
  return {
    id,
    tool: "php",
    version: v,
    canonical_name: `${v}-nts-vs17-x64`,
    variant: {},
    state: "installed",
    source: "registry",
    path: `C:/Forgebox/bin/php/${v}`,
    size_bytes: null,
    installed_at: null,
    is_current: false,
    ...patch,
  };
}

const V83 = version("01JVERS00000000000000000A1", "8.3.36");
const V84 = version("01JVERS00000000000000000A2", "8.4.1", { is_current: true });

const INFO: ToolInfoDto = {
  tool: {
    id: "php",
    name: "PHP",
    category: "runtime",
    description: null,
    installed: [V83, V84],
    current: "8.4.1",
    policy: "notify",
    latest: null,
    update_available: null,
    has_service: false,
  },
  available: [],
  used_by: [{ site_id: "01JSITE00000000000000000LJA", host: "loja.test", version: "8.4" }],
};

const HISTORY: UpdateHistoryDto = {
  tool: "php",
  entries: [
    {
      id: "01JUPDT000000000000000001",
      series: "8.3",
      from: "8.3.35",
      to: "8.3.36",
      trigger: "user",
      status: "completed",
      started_at: "2026-10-20T09:00:00Z",
    },
  ],
};

const versionHandlers = {
  "tools.info": () => INFO,
  "tools.history": () => HISTORY,
};

describe("Versoes", () => {
  it("disables remove for a version in use, with the reason, and labels the CLI version", async () => {
    await mount(<VersionsTab tool="php" />, { handlers: versionHandlers });
    const inUseRow = await screen.findByText("8.4.1").then((el) => el.closest("li"));
    const free = document.querySelector<HTMLElement>('[data-version="8.3.36"]');
    expect(inUseRow).toHaveAttribute("data-in-use", "sites");
    const remove = within(inUseRow as HTMLElement).getByRole("button", { name: "Remove 8.4.1" });
    expect(remove).toBeDisabled();
    // The wrapper takes focus and carries the reason (also the tooltip text).
    expect(must(remove.parentElement).getAttribute("data-disabled-reason")).toBe(
      "In use by 1 site.",
    );
    expect(within(inUseRow as HTMLElement).getByText("CLI")).toBeInTheDocument();
    expect(
      screen.getByText(/only changes the version that the forge command line/),
    ).toBeInTheDocument();
    expect(within(must(free)).getByRole("button", { name: "Remove 8.3.36" })).toBeEnabled();
  });

  it("shows the exact CLI strings for use, remove and rollback", async () => {
    const user = userEvent.setup();
    const h = await mount(<VersionsTab tool="php" />, {
      handlers: {
        ...versionHandlers,
        "tools.use": () => ({ ...V83, is_current: true }),
        "tools.remove": () => fakeJob({ outcome: { done: { ok: true, message: null } } }),
      },
    });
    const row = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-version="8.3.36"]');
      if (el === null) throw new Error("not yet");
      return el;
    });
    expect(action("tools.use_version", row)).toHaveAttribute("data-forge-cli", "forge use php 8.3");
    // The CLI version cannot be selected again.
    const current = must(document.querySelector<HTMLElement>('[data-version="8.4.1"]'));
    expect(action("tools.use_version", current)).toHaveAttribute("data-disabled-reason");

    await user.click(within(row).getByRole("button", { name: "Remove 8.3.36" }));
    const confirm = await screen.findByTestId("remove-version-confirm");
    expect(action("tools.remove_version", confirm)).toHaveAttribute(
      "data-forge-cli",
      "forge tool remove php@8.3.36",
    );
    await user.click(within(confirm).getByRole("button", { name: "Remove version" }));
    await waitFor(() => {
      expect(h.fake.calls.find((c) => c.method === "tools.remove")?.params).toEqual({
        spec: "php@8.3.36",
        confirm: true,
      });
    });

    expect(await screen.findByRole("button", { name: "Roll back" })).toBeInTheDocument();
    expect(action("tools.rollback_series")).toHaveAttribute(
      "data-forge-cli",
      "forge tool rollback php@8.3",
    );
    expect(action("tools.check_updates_one")).toHaveAttribute(
      "data-forge-cli",
      "forge tool update php --check",
    );
  });

  it("changes the policy and reports what a check found", async () => {
    const user = userEvent.setup();
    const h = await mount(<VersionsTab tool="php" />, {
      handlers: {
        ...versionHandlers,
        "tools.check_updates": () =>
          fakeJob({
            outcome: {
              done: { updates: [{ tool: "php", from: "8.3.36", to: "8.3.37", automatic: false }] },
            },
          }),
      },
    });
    expect(await screen.findByRole("combobox", { name: "Update policy" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByTestId("update-result")).toHaveTextContent("8.3.36 → 8.3.37");
    expect(h.count("tools.check_updates")).toBe(1);
  });
});

describe("versionsModel", () => {
  it("uses the series when it is unambiguous, else the exact version", () => {
    const other = version("01JVERS00000000000000000A3", "8.3.30");
    expect(cliSpecFor("8.4.1", [V83, V84])).toBe("8.4");
    expect(cliSpecFor("8.3.36", [V83, other])).toBe("8.3.36");
    expect(cliSpecFor("22", [version("x", "22")])).toBe("22");
  });

  it("blocks a version used by an instance first, then sites, then the CLI", () => {
    const instance = {
      id: "i",
      tool: "php",
      version_id: V83.id,
      name: "x",
    } as unknown as Parameters<typeof inUse>[1][number];
    expect(inUse(V83, [instance], [])).toEqual({ kind: "instance", names: ["php/x"] });
    expect(inUse(V84, [], INFO.used_by)).toEqual({ kind: "sites", count: 1 });
    expect(inUse(V84, [], [])).toEqual({ kind: "cli" });
    expect(inUse(V83, [], [])).toBeNull();
  });

  it("offers a rollback only for the newest completed update of a series", () => {
    const entry = HISTORY.entries[0];
    if (entry === undefined) throw new Error("fixture");
    const rolled = { ...entry, id: "r", rollback_of: entry.id, status: "completed" as const };
    expect([...rollbackCandidates([entry]).keys()]).toEqual(["8.3"]);
    expect([...rollbackCandidates([rolled, entry]).keys()]).toEqual([]);
    expect([...rollbackCandidates([{ ...entry, status: "failed" }]).keys()]).toEqual([]);
  });
});

describe("Avancado", () => {
  const run = async (settings: ServiceSettingsDto) => {
    const updated = vi.fn();
    const h = await mount(<AdvancedTab target="mysql/default" version="8.4.3" />, {
      handlers: {
        "services.settings": () => settings,
        "services.update": (params) => {
          updated(params);
          return fakeJob({ outcome: { done: harness?.data.instances[0] as never } });
        },
      },
    });
    return { h, updated };
  };

  it("sends services.update with only the changed fields", async () => {
    const user = userEvent.setup();
    const { h, updated } = await run(SETTINGS);
    const args = await screen.findByRole("textbox", { name: "Extra arguments" });
    expect(action("services.update_instance")).toHaveAttribute(
      "data-disabled-reason",
      "Nothing changed yet.",
    );
    fireEvent.change(args, { target: { value: "--skip-name-resolve\n--bind-address=127.0.0.1" } });
    await user.click(screen.getByRole("switch", { name: "Start with Forgebox" }));
    await user.click(
      within(action("services.update_instance")).getByRole("button", {
        name: "Save advanced settings",
      }),
    );
    await waitFor(() => {
      expect(h.count("services.update")).toBe(1);
    });
    expect(updated).toHaveBeenCalledWith({
      instance: "mysql/default",
      autostart: true,
      launch: { args: ["--skip-name-resolve", "--bind-address=127.0.0.1"] },
    });
  });

  it("shows the CLI of an autostart change and refuses a bad environment line", async () => {
    const user = userEvent.setup();
    await run(SETTINGS);
    await user.click(await screen.findByRole("switch", { name: "Start with Forgebox" }));
    expect(action("services.update_instance")).toHaveAttribute(
      "data-forge-cli",
      "forge service update mysql/default --autostart",
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Environment variables" }), {
      target: { value: "NOT AN ASSIGNMENT" },
    });
    expect(screen.getByText("Use KEY=value, one per line.")).toBeInTheDocument();
    expect(action("services.update_instance")).toHaveAttribute(
      "data-disabled-reason",
      "Fix the highlighted fields first.",
    );
  });

  it("explains that it needs an administrator when the daemon sends no advanced block", async () => {
    await run({ ...SETTINGS, advanced: null });
    expect(await screen.findByTestId("advanced-unavailable")).toBeInTheDocument();
  });

  it("builds a patch of the restart policy and version fields too", () => {
    const advanced: InstanceAdvancedDto = must(SETTINGS.advanced);
    const base = draftOf(advanced, "8.4.3");
    expect(advancedPatch(base, base)).toEqual({});
    expect(
      advancedPatch(base, {
        ...base,
        on: "always",
        max: "5",
        windowSecs: "120",
        version: "8.4.2",
        env: "A=1",
      }),
    ).toEqual({
      launch: { env: { A: "1" } },
      restart_policy: { on: "always", max: 5, window_secs: 120 },
      version: "8.4.2",
    });
  });
});
