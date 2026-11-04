// Acceptance of M2-E3-T18: Settings > Sistema de arquivos, Registries and Atualizacoes over the
// fake bridge (fixtures in qa/fixtures/scenarios/filesystem.ts).
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import {
  createFilesystemQa,
  QA_LINT_ROOT,
  type FilesystemQa,
} from "../../../qa/fixtures/scenarios/filesystem";
import { settingsHandlers } from "../../../qa/fixtures/scenarios/settings";
import { renderApp, type AppHarness } from "../../app/testApp";
import type { FakeHandlers } from "../../ipc/fakeBridge";

let harness: AppHarness | null = null;

afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

type SectionName = "filesystem" | "registries" | "updates";

async function open(
  section: SectionName,
  options: { qa?: FilesystemQa; handlers?: FakeHandlers } = {},
) {
  const qa = options.qa ?? createFilesystemQa();
  const config = settingsHandlers();
  harness = await renderApp({
    path: "/settings",
    extra: { handlers: { ...config.handlers, ...qa.handlers, ...options.handlers } },
  });
  const user = userEvent.setup();
  await user.click(await screen.findByTestId(`settings-nav-${section}`));
  return { h: harness, qa, user, config };
}

const calls = (h: AppHarness, method: string) => h.fake.calls.filter((c) => c.method === method);

async function pick(user: ReturnType<typeof userEvent.setup>, name: string, option: string) {
  await user.click(await screen.findByRole("combobox", { name }));
  await user.click(await screen.findByRole("option", { name: option }));
}

describe("Sistema de arquivos", () => {
  it("lists the state of every folder with the reason when unsupported", async () => {
    const { h, user } = await open("filesystem");
    const table = await screen.findByTestId("setting-fs-table");
    await waitFor(() => {
      expect(within(table).getByTestId("case-row-data")).toHaveAttribute("data-state", "disabled");
    });
    expect(within(table).getByTestId("case-row-www")).toHaveAttribute("data-state", "enabled");
    expect(within(table).getByTestId("case-row-tmp")).toHaveAttribute("data-state", "unsupported");
    expect(within(table).getByTestId("case-reason-tmp")).toHaveTextContent(/needs NTFS/);
    expect(within(table).getByText(/forge fs case-sensitive status .*\/www/)).toBeInTheDocument();
    // The global toggle is the `case_sensitive` row of the configuration.
    expect(
      within(await screen.findByTestId("setting-case_sensitive")).getByRole("switch"),
    ).toBeChecked();
    // Enabling a folder is one call with the exact command in the tooltip wrapper.
    const row = within(table).getByTestId("case-row-data");
    await user.click(within(row).getByRole("button", { name: "Enable" }));
    await waitFor(() => {
      expect(within(table).getByTestId("case-row-data")).toHaveAttribute("data-state", "enabled");
    });
    expect(calls(h, "fs_parity.case_enable")).toHaveLength(1);
  });

  it("converts with progress, then reports and refreshes the table", async () => {
    const { h, user } = await open("filesystem");
    const card = await screen.findByTestId("setting-fs-convert");
    await pick(user, "Folder to convert", "data/");
    await user.click(within(card).getByTestId("fs-convert-run"));
    const status = await within(card).findByTestId("fs-convert-status");
    expect(status).toHaveAttribute("data-phase", "running");
    expect(within(status).getByRole("progressbar")).toBeInTheDocument();
    await waitFor(() => {
      expect(within(card).getByTestId("fs-convert-report")).toHaveTextContent(
        "1203 files in 48 folders",
      );
    });
    expect(calls(h, "fs_parity.case_convert")[0]?.params).toMatchObject({
      dir: expect.stringMatching(/\/data$/) as string,
    });
    await waitFor(() => {
      expect(screen.getByTestId("case-row-data")).toHaveAttribute("data-state", "enabled");
    });
  });

  it("cancels a running conversion and says nothing was moved", async () => {
    const qa = createFilesystemQa({ convert: "hold" });
    const { h, user } = await open("filesystem", { qa });
    const card = await screen.findByTestId("setting-fs-convert");
    await user.click(within(card).getByTestId("fs-convert-run"));
    await user.click(await within(card).findByTestId("fs-convert-cancel"));
    await waitFor(() => {
      expect(within(card).getByTestId("fs-convert-status")).toHaveAttribute(
        "data-phase",
        "cancelled",
      );
    });
    expect(within(card).getByRole("status")).toHaveTextContent(/left as it was/);
    expect(calls(h, "jobs.cancel")).toHaveLength(1);
  });

  it("asks before reverting and explains a case conflict", async () => {
    const qa = createFilesystemQa({ convert: "conflict" });
    const { h, user } = await open("filesystem", { qa });
    const card = await screen.findByTestId("setting-fs-convert");
    await user.click(within(card).getByTestId("fs-convert-revert"));
    const dialog = await screen.findByTestId("fs-revert-dialog");
    expect(within(dialog).getByText(/forge fs case-sensitive convert .* --revert/)).toBeVisible();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(calls(h, "fs_parity.case_convert")).toHaveLength(0);

    await user.click(within(card).getByTestId("fs-convert-revert"));
    await user.click(
      within(await screen.findByTestId("fs-revert-dialog")).getByRole("button", {
        name: "Revert to insensitive",
      }),
    );
    const conflict = await within(card).findByTestId("fs-convert-conflict");
    expect(conflict).toHaveTextContent(/differ only by case/);
    expect(calls(h, "fs_parity.case_convert")[0]?.params).toMatchObject({ revert: true });
  });

  it("does not blame case duplicates when the revert conflicts because a file is in use", async () => {
    const qa = createFilesystemQa({ convert: "in_use" });
    const { user } = await open("filesystem", { qa });
    const card = await screen.findByTestId("setting-fs-convert");
    await user.click(within(card).getByTestId("fs-convert-revert"));
    await user.click(
      within(await screen.findByTestId("fs-revert-dialog")).getByRole("button", {
        name: "Revert to insensitive",
      }),
    );
    await waitFor(() => {
      expect(within(card).getByTestId("fs-convert-status")).toHaveAttribute("data-phase", "failed");
    });
    expect(within(card).queryByTestId("fs-convert-conflict")).toBeNull();
  });

  it("scans as a dry run, opens the editor at file:line and applies only after confirm", async () => {
    const { h, user } = await open("filesystem");
    const card = await screen.findByTestId("setting-fs-finder");
    await waitFor(() => {
      expect(within(card).getByTestId("finder-scan")).toBeEnabled();
    });
    await user.click(within(card).getByTestId("finder-scan"));
    const report = await within(card).findByTestId("finder-report");
    expect(report).toHaveAttribute("data-applied", "false");
    expect(calls(h, "fs_parity.lint_case")[0]?.params).toMatchObject({
      fix: true,
      dry_run: true,
      strategy: "auto",
    });
    expect(within(report).getByTestId("finding-open-app/index.php:12:9")).toHaveTextContent(
      "app/index.php:12",
    );
    expect(within(report).getByTestId("finding-fix-app/index.php:12:9")).toHaveTextContent(
      "Rename file",
    );

    await user.click(within(report).getByTestId("finding-open-app/index.php:12:9"));
    await waitFor(() => {
      expect(calls(h, "shell.open_editor")).toHaveLength(1);
    });
    expect(calls(h, "shell.open_editor")[0]?.params).toEqual({
      path: `${QA_LINT_ROOT}/app/index.php`,
      line: 12,
      column: 9,
    });

    // Apply opens the confirmation; nothing is written until it is confirmed.
    await user.click(within(card).getByTestId("finder-apply"));
    const dialog = await screen.findByTestId("finder-dialog");
    expect(within(dialog).getByText(/--strategy auto/)).toBeVisible();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(calls(h, "fs_parity.lint_case")).toHaveLength(1);

    await user.click(within(card).getByTestId("finder-apply"));
    await user.click(
      within(await screen.findByTestId("finder-dialog")).getByRole("button", { name: "Apply" }),
    );
    await waitFor(() => {
      expect(calls(h, "fs_parity.lint_case")).toHaveLength(2);
    });
    const applied = calls(h, "fs_parity.lint_case")[1]?.params as Record<string, unknown>;
    expect(applied.fix).toBe(true);
    expect(applied.dry_run).toBeUndefined();
    await waitFor(() => {
      expect(within(card).getByTestId("finder-report")).toHaveAttribute("data-applied", "true");
    });
  });
});

describe("Registries", () => {
  it("shows the badges, the last refresh and the missing refresh", async () => {
    await open("registries");
    const official = await screen.findByTestId("registry-official");
    expect(official).toHaveAttribute("data-trust", "official");
    expect(within(official).getByText("Official")).toBeInTheDocument();
    expect(within(official).queryByRole("button", { name: "Remove" })).toBeNull();
    expect(within(official).getByTestId("registry-refresh-official")).toHaveTextContent(
      /refreshed/,
    );
    expect(screen.getByTestId("registry-acme")).toHaveAttribute("data-trust", "third-party");
    const lab = screen.getByTestId("registry-lab");
    expect(lab).toHaveAttribute("data-trust", "unverified");
    expect(within(lab).getByTestId("registry-refresh-lab")).toHaveTextContent("never refreshed");
    expect(within(lab).getByTestId("registry-error-lab")).toBeInTheDocument();
    // A signed dir mirror is not "unverified" just because it is a folder.
    expect(screen.getByTestId("registry-mirror")).toHaveAttribute("data-trust", "third-party");
    // Refreshed once and failing since: the error shows next to the last refresh.
    const flaky = screen.getByTestId("registry-flaky");
    expect(within(flaky).getByTestId("registry-refresh-flaky")).toHaveTextContent(/refreshed/);
    expect(within(flaky).getByRole("alert")).toHaveTextContent(/HTTP 503/);
    expect(within(screen.getByTestId("registry-acme")).queryByRole("alert")).toBeNull();
  });

  it("refuses an invalid URL inline without calling the daemon", async () => {
    const { h, user } = await open("registries");
    const field = await screen.findByRole("textbox", { name: "Registry URL" });
    await user.type(field, "ftp://nope");
    await user.tab();
    expect((await screen.findAllByText(/Use an https:\/\/ URL/)).length).toBeGreaterThan(0);
    const add = screen.getByRole("button", { name: "Add registry" });
    expect(add).toBeDisabled();
    expect(calls(h, "registry.add")).toHaveLength(0);
  });

  it("adds a valid URL with the exact command and lists it", async () => {
    const { h, user } = await open("registries");
    const field = await screen.findByRole("textbox", { name: "Registry URL" });
    await user.type(field, "https://tools.example.org/index.yaml");
    await user.type(screen.getByRole("textbox", { name: "Name (optional)" }), "tools");
    const add = screen.getByRole("button", { name: "Add registry" });
    expect(add.closest("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge registry add https://tools.example.org/index.yaml --name tools",
    );
    await user.click(add);
    await waitFor(() => {
      expect(calls(h, "registry.add")).toHaveLength(1);
    });
    expect(calls(h, "registry.add")[0]?.params).toEqual({
      url: "https://tools.example.org/index.yaml",
      name: "tools",
    });
    expect(await screen.findByTestId("registry-tools")).toBeInTheDocument();
  });

  it("asks before removing; cancel calls nothing, confirm removes", async () => {
    const { h, user } = await open("registries");
    const row = await screen.findByTestId("registry-acme");
    await user.click(within(row).getByRole("button", { name: "Remove" }));
    const dialog = await screen.findByTestId("registry-remove-dialog");
    expect(within(dialog).getByText("forge registry remove acme")).toBeVisible();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(calls(h, "registry.remove")).toHaveLength(0);

    await user.click(within(row).getByRole("button", { name: "Remove" }));
    await user.click(
      within(await screen.findByTestId("registry-remove-dialog")).getByRole("button", {
        name: "Remove",
      }),
    );
    await waitFor(() => {
      expect(screen.queryByTestId("registry-acme")).toBeNull();
    });
    expect(calls(h, "registry.remove")[0]?.params).toEqual({ name: "acme" });
  });

  it("runs the update job with progress and refreshes the dates", async () => {
    const { h, user } = await open("registries");
    const button = await screen.findByTestId("registry-update");
    expect(button.closest("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge registry update",
    );
    await user.click(button);
    const status = await screen.findByTestId("registry-update-status");
    expect(status).toHaveAttribute("data-phase", "running");
    expect(within(status).getByRole("progressbar")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByTestId("registry-update-status")).toHaveAttribute("data-phase", "done");
    });
    expect(calls(h, "registry.update")).toHaveLength(1);
    await waitFor(() => {
      expect(screen.getByTestId("registry-refresh-acme")).toHaveTextContent(/now|seconds? ago/);
    });
  });
});

describe("Atualizacoes", () => {
  it("shows the app channel and self-update disabled with the 1.0 note", async () => {
    await open("updates");
    const app = await screen.findByTestId("update-app-inactive");
    expect(within(app).getByRole("combobox", { name: "Channel" })).toBeDisabled();
    expect(within(app).getByRole("button", { name: "Update Forgebox" })).toBeDisabled();
    expect(app).toHaveTextContent(/arrive in 1\.0/);
  });

  it("validates quiet hours and saves the pair as config", async () => {
    const { h, user, config } = await open("updates");
    const row = await screen.findByTestId("setting-update-quiet");
    const save = within(row).getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    const start = within(row).getByRole("textbox", { name: "From" });
    const end = within(row).getByRole("textbox", { name: "Until" });
    await user.type(start, "25:00");
    await user.type(end, "07:00");
    expect(within(row).getAllByText(/Use HH:MM/).length).toBeGreaterThan(0);
    expect(save).toBeDisabled();
    await user.clear(start);
    await user.type(start, "22:00");
    expect(within(row).queryByText(/Use HH:MM/)).toBeNull();
    await user.click(save);
    await waitFor(() => {
      expect(calls(h, "system.config_set")).toHaveLength(1);
    });
    expect(calls(h, "system.config_set")[0]?.params).toEqual({
      key: "updates.quiet_hours",
      value: { start: "22:00", end: "07:00" },
    });
    expect(config.values().updates.quiet_hours).toEqual({ start: "22:00", end: "07:00" });
  });

  it("refuses a half pair", async () => {
    const { h, user } = await open("updates");
    const row = await screen.findByTestId("setting-update-quiet");
    await user.type(within(row).getByRole("textbox", { name: "From" }), "22:00");
    expect(within(row).getAllByText(/both times/).length).toBeGreaterThan(0);
    expect(within(row).getByRole("button", { name: "Save" })).toBeDisabled();
    expect(calls(h, "system.config_set")).toHaveLength(0);
  });

  it("saves the metered policy with a localized option", async () => {
    const { h, user } = await open("updates");
    await screen.findByTestId("setting-update-quiet");
    await pick(user, "Metered connection", "Download anyway");
    await waitFor(() => {
      expect(calls(h, "system.config_set")).toHaveLength(1);
    });
    expect(calls(h, "system.config_set")[0]?.params).toEqual({
      key: "updates.metered",
      value: "allow",
    });
  });

  it("changes a tool policy through tools.policy with the exact command", async () => {
    const { h, user } = await open("updates");
    const row = await screen.findByTestId("policy-php");
    expect(within(row).getByText("forge tool policy php notify")).toBeInTheDocument();
    await user.click(within(row).getByRole("combobox", { name: "Update policy of PHP" }));
    await user.click(await screen.findByRole("option", { name: "Patch automatically" }));
    await waitFor(() => {
      expect(calls(h, "tools.policy")).toHaveLength(1);
    });
    expect(calls(h, "tools.policy")[0]?.params).toEqual({ tool: "php", policy: "auto-patch" });
    expect(await within(row).findByText("forge tool policy php auto-patch")).toBeInTheDocument();
    expect(within(row).getByText("8.4.27 available")).toBeInTheDocument();
  });

  it("checks now as a job and reports what it found", async () => {
    const { h, user } = await open("updates");
    const button = await screen.findByTestId("update-check-now");
    expect(button.closest("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge tool update --check",
    );
    await user.click(button);
    expect(await screen.findByTestId("update-check-status")).toHaveAttribute(
      "data-phase",
      "running",
    );
    expect(await screen.findByTestId("update-check-result")).toHaveTextContent(
      "1 tool has an update",
    );
    expect(calls(h, "tools.check_updates")[0]?.params).toEqual({});
  });
});
