// Acceptance of M2-E3-T19 (docs/07 section 5.12): idle / running / done / failed, the fix
// allowlist (and "Copiar comando" for anything else), Corrigir -> fix -> re-run of the group,
// Encerrar with its confirmation (hidden on remote), Testar DNS and the exported report.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createDiagnosticsQa,
  QA_HOLDER,
  type DiagnosticsQa,
} from "../../../qa/fixtures/scenarios/diagnostics";
import { renderApp, type AppHarness } from "../../app/testApp";
import { fakeError, fakeJob } from "../../ipc/fakeBridge";

const native = vi.hoisted(() => ({
  copyText: vi.fn(() => Promise.resolve(true)),
  pasteText: vi.fn(() => Promise.resolve(null)),
  pickPath: vi.fn(() => Promise.resolve(null)),
  openUrl: vi.fn(() => Promise.resolve(true)),
  revealPath: vi.fn(() => Promise.resolve(true)),
  onPathsDropped: vi.fn(() => Promise.resolve(() => undefined)),
  saveTextFile: vi.fn<(name: string, contents: string) => Promise<{ saved: string }>>(() =>
    Promise.resolve({ saved: "C:/tmp/report.json" }),
  ),
}));
vi.mock("../../ipc/native", () => native);

let harness: AppHarness | null = null;

beforeEach(() => {
  Object.values(native).forEach((spy) => spy.mockClear());
});

afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

/**
 * `holdJobs`: methods whose jobs run until the test ends them (`FakeBridge.completeJob`), so a
 * test that checks the running state does not race the script's step timers on a loaded machine.
 */
async function open(
  qa: DiagnosticsQa = createDiagnosticsQa(),
  options: { scenario?: string; holdJobs?: readonly string[] } = {},
): Promise<{ h: AppHarness; qa: DiagnosticsQa }> {
  harness = await renderApp({
    path: "/diagnostics",
    ...(options.scenario === undefined ? {} : { scenario: options.scenario }),
    extra: {
      notImplemented: [],
      latencyMs: 0,
      handlers: qa.handlers,
      holdJobs: options.holdJobs ?? [],
    },
  });
  return { h: harness, qa };
}

const calls = (method: string) =>
  harness?.fake.calls.filter((call) => call.method === method) ?? [];

function row(id: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-check="${id}"]`);
  if (el === null) throw new Error(`no row ${id}`);
  return el;
}

async function run(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Run diagnostics" }));
  await screen.findByTestId("doctor-summary");
}

describe("states", () => {
  it("starts idle, with the equivalent command and nothing run", async () => {
    await open();
    expect(await screen.findByText("Nothing checked yet")).toBeInTheDocument();
    expect(document.querySelector('[data-forge-cli="forge doctor"]')).not.toBeNull();
    expect(screen.getByRole("button", { name: "Export report" })).toBeDisabled();
    expect(calls("daemon.doctor")).toHaveLength(0);
    expect(screen.getByTestId("page-diagnostics")).toHaveAttribute("data-phase", "idle");
  });

  it("shows the progress step while running and the grouped results when done", async () => {
    const user = userEvent.setup();
    const { h } = await open(undefined, { holdJobs: ["daemon.doctor"] });
    await user.click(await screen.findByRole("button", { name: "Run diagnostics" }));
    expect(screen.getByTestId("page-diagnostics")).toHaveAttribute("data-phase", "running");
    expect(await screen.findByText(/^Checking: /)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Running…" })).toBeDisabled();
    const job = h.fake.jobList().find((j) => j.name === "daemon.doctor" && j.state === "running");
    if (job === undefined) throw new Error("no running daemon.doctor job");
    act(() => {
      expect(h.fake.completeJob(job.id)).toBe(true);
      h.fake.flush();
    });
    await screen.findByTestId("doctor-summary");
    expect(screen.getByTestId("doctor-summary")).toHaveTextContent(
      "4 ok · 4 warnings · 1 failures · 1 skipped",
    );
    const groups = [...document.querySelectorAll<HTMLElement>("[data-group]")].map(
      (el) => el.dataset.group,
    );
    expect(groups).toEqual(["root", "store", "dns", "tls", "port", "path", "defender", "service"]);
    expect(within(row("port.80")).getByText("Attention")).toBeInTheDocument();
    expect(within(row("defender.exclusion")).getByText("Skipped")).toBeInTheDocument();
    expect(calls("daemon.doctor")[0]?.params).toEqual({});
  });

  it("shows the failure with a way to try again", async () => {
    const user = userEvent.setup();
    const qa = createDiagnosticsQa();
    qa.handlers["daemon.doctor"] = () =>
      fakeJob({ outcome: { failed: { code: "internal", message: "doctor crashed" } } });
    await open(qa);
    await user.click(await screen.findByRole("button", { name: "Run diagnostics" }));
    expect(await screen.findByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(screen.getByTestId("page-diagnostics")).toHaveAttribute("data-phase", "failed");
  });
});

describe("fixes", () => {
  it("offers Fix only for the allowlist and only the command for anything else", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    for (const id of ["dns.policy.test", "tls.ca_trusted", "path.user"]) {
      expect(within(row(id)).getByRole("button", { name: "Fix" })).toBeEnabled();
    }
    // `daemon.stop` is not an allowed fix_method: no Fix, the command to copy instead.
    const nginx = row("service.nginx");
    expect(within(nginx).queryByRole("button", { name: "Fix" })).toBeNull();
    expect(nginx.querySelector('[data-forge-cli="forge daemon stop"]')).not.toBeNull();
    await user.click(within(nginx).getByRole("button", { name: "Copy command" }));
    expect(native.copyText).toHaveBeenCalledWith("forge daemon stop");
    // A conflict with only a hint (`forge service ports`) also gets just the command.
    expect(within(row("port.80")).queryByRole("button", { name: "Fix" })).toBeNull();
  });

  it("Fix on dns.policy runs net.dns_enable, then doctor {checks:['dns']}, and the row updates", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    await user.click(within(row("dns.policy.test")).getByRole("button", { name: "Fix" }));
    await waitFor(() => {
      expect(row("dns.policy.test")).toHaveAttribute("data-status", "ok");
    });
    expect(calls("net.dns_enable")).toHaveLength(1);
    expect(calls("daemon.doctor").map((call) => call.params)).toEqual([{}, { checks: ["dns"] }]);
    // The order: the fix first, then the re-run.
    const order = harness?.fake.calls.map((call) => call.method) ?? [];
    expect(order.indexOf("net.dns_enable")).toBeLessThan(order.lastIndexOf("daemon.doctor"));
    // Other groups were not touched.
    expect(row("path.user")).toHaveAttribute("data-status", "warn");
    expect(within(row("dns.policy.test")).queryByRole("button", { name: "Fix" })).toBeNull();
  });

  it("passes fix_params to tls.ca_install and runs shell.path_add without params", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    await user.click(within(row("tls.ca_trusted")).getByRole("button", { name: "Fix" }));
    await waitFor(() => {
      expect(row("tls.ca_trusted")).toHaveAttribute("data-status", "ok");
    });
    expect(calls("tls.ca_install")[0]?.params).toEqual({ scope: "current_user" });
    await user.click(within(row("path.user")).getByRole("button", { name: "Fix" }));
    await waitFor(() => {
      expect(row("path.user")).toHaveAttribute("data-status", "ok");
    });
    expect(calls("shell.path_add")[0]?.params).toEqual({});
    expect(calls("daemon.doctor").map((c) => c.params)).toEqual([
      {},
      { checks: ["tls"] },
      { checks: ["path"] },
    ]);
  });
});

describe("port conflicts", () => {
  it("offers Encerrar and Trocar porta only on the conflicting port", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    const conflict = within(row("port.80"));
    expect(conflict.getByRole("button", { name: "End process" })).toBeEnabled();
    expect(conflict.getByRole("button", { name: "Change port" })).toBeEnabled();
    expect(within(row("port.443")).queryByRole("button", { name: "End process" })).toBeNull();
  });

  it("confirms with the pid and exe, sends confirm, pid and port, then re-runs the ports", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    await user.click(within(row("port.80")).getByRole("button", { name: "End process" }));
    const dialog = await screen.findByTestId("kill-port-dialog");
    const holder = await within(dialog).findByTestId("kill-holder");
    expect(holder).toHaveTextContent(String(QA_HOLDER.pid));
    expect(holder).toHaveTextContent("httpd.exe");
    expect(calls("services.kill_port_holder")).toHaveLength(0);
    await user.click(within(dialog).getByRole("button", { name: "End process" }));
    await waitFor(() => {
      expect(row("port.80")).toHaveAttribute("data-status", "ok");
    });
    expect(calls("services.kill_port_holder")[0]?.params).toEqual({
      port: 80,
      pid: QA_HOLDER.pid,
      confirm: true,
    });
    expect(calls("daemon.doctor").map((c) => c.params)).toEqual([{}, { checks: ["port"] }]);
    await waitFor(() => {
      expect(screen.queryByTestId("kill-port-dialog")).toBeNull();
    });
  });

  it("cancel sends nothing", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    await user.click(within(row("port.80")).getByRole("button", { name: "End process" }));
    const dialog = await screen.findByTestId("kill-port-dialog");
    await within(dialog).findByTestId("kill-holder");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.queryByTestId("kill-port-dialog")).toBeNull();
    });
    expect(calls("services.kill_port_holder")).toHaveLength(0);
  });

  it("shows the daemon's refusal in the dialog", async () => {
    const user = userEvent.setup();
    const qa = createDiagnosticsQa();
    qa.handlers["services.kill_port_holder"] = () => {
      fakeError("conflict", "that process does not hold the port", "not_the_holder");
    };
    await open(qa);
    await run(user);
    await user.click(within(row("port.80")).getByRole("button", { name: "End process" }));
    const dialog = await screen.findByTestId("kill-port-dialog");
    await within(dialog).findByTestId("kill-holder");
    await user.click(within(dialog).getByRole("button", { name: "End process" }));
    expect(await within(dialog).findByRole("alert")).toBeInTheDocument();
    expect(screen.getByTestId("kill-port-dialog")).toBeInTheDocument();
  });

  it("hides Encerrar on a remote daemon, keeping Trocar porta", async () => {
    const user = userEvent.setup();
    await open(createDiagnosticsQa(), { scenario: "remote" });
    await run(user);
    const conflict = within(row("port.80"));
    expect(conflict.queryByRole("button", { name: "End process" })).toBeNull();
    expect(conflict.getByRole("button", { name: "Change port" })).toBeInTheDocument();
  });
});

describe("DNS test and export", () => {
  it("Testar DNS asks net.dns_test for a name under the first TLD", async () => {
    const user = userEvent.setup();
    const qa = createDiagnosticsQa();
    // In the QA scenarios `net.dns_test` is the Site detail's handler; the unit test brings its own.
    qa.handlers["net.dns_test"] = (params) => ({
      host: params.host,
      resolved: ["127.0.0.1"],
      expected: ["127.0.0.1"],
      ok: true,
      via: "forgebox-dns",
    });
    await open(qa);
    await user.click(await screen.findByRole("button", { name: "Test DNS" }));
    await waitFor(() => {
      expect(document.querySelector("[data-dns-ok]")).not.toBeNull();
    });
    expect(calls("net.dns_test")[0]?.params).toEqual({ host: "forgebox-doctor.test" });
    expect(screen.getByTestId("dns-test")).toHaveTextContent("forgebox-doctor.test resolves to");
  });

  it("exports the CLI bundle through the save dialog", async () => {
    const user = userEvent.setup();
    await open();
    await run(user);
    await user.click(screen.getByRole("button", { name: "Export report" }));
    await waitFor(() => {
      expect(native.saveTextFile).toHaveBeenCalledTimes(1);
    });
    const [name, text] = native.saveTextFile.mock.calls[0] ?? [];
    expect(name).toMatch(/^forgebox-diagnostics-\d{8}-\d{6}\.json$/);
    const bundle = JSON.parse(String(text)) as Record<string, unknown>;
    expect(Object.keys(bundle)).toEqual([
      "v",
      "created_unix",
      "forge_version",
      "api_version",
      "doctor",
      "status",
      "tools",
      "daemon_log",
    ]);
    expect(bundle.api_version).toBe("1.1");
    expect((bundle.doctor as { checks: unknown[] }).checks).toHaveLength(10);
    expect(bundle.daemon_log).toEqual(["forgeboxd started", "port 80 busy"]);
    expect(calls("logs.read")[0]?.params).toEqual({ target: "daemon", limit: 500 });
  });

  it("still exports when the daemon log cannot be read", async () => {
    const user = userEvent.setup();
    const qa = createDiagnosticsQa();
    qa.handlers["logs.read"] = () => {
      fakeError("not_found", "no log");
    };
    await open(qa);
    await run(user);
    await act(async () => {
      await user.click(screen.getByRole("button", { name: "Export report" }));
    });
    await waitFor(() => {
      expect(native.saveTextFile).toHaveBeenCalledTimes(1);
    });
    const bundle = JSON.parse(String(native.saveTextFile.mock.calls[0]?.[1])) as {
      daemon_log: string[];
    };
    expect(bundle.daemon_log[0]).toMatch(/^\(daemon log unavailable: /);
  });
});
