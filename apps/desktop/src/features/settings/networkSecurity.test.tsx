import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  QA_PEM,
  createNetworkSecurityQa,
  type NetworkSecurityQa,
} from "../../../qa/fixtures/scenarios/networkSecurity";
import { settingsHandlers } from "../../../qa/fixtures/scenarios/settings";
import { renderApp, type AppHarness } from "../../app/testApp";
import { toast, toastStore } from "../../design/primitives/toastStore";
import { fakeError, type FakeHandlers } from "../../ipc/fakeBridge";

const native = vi.hoisted(() => ({
  saveCaCertificate: vi.fn<
    (name: string, pem: string) => Promise<{ saved: string } | { cancelled: true }>
  >(() => Promise.resolve({ saved: "C:/Users/me/ca.crt" })),
  copyText: vi.fn(() => Promise.resolve(true)),
}));
vi.mock("../../ipc/native", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc/native")>()),
  ...native,
}));

let harness: AppHarness | null = null;

afterEach(async () => {
  native.saveCaCertificate.mockClear();
  for (const item of toastStore.getSnapshot()) toast.dismiss(item.id);
  await harness?.layer.dispose();
  harness = null;
});

async function open(
  section: "network" | "security",
  options: {
    qa?: NetworkSecurityQa;
    handlers?: FakeHandlers;
  } = {},
) {
  const qa = options.qa ?? createNetworkSecurityQa();
  const config = settingsHandlers();
  harness = await renderApp({
    path: "/settings",
    extra: { handlers: { ...config.handlers, ...qa.handlers, ...options.handlers } },
  });
  const user = userEvent.setup();
  await user.click(await screen.findByTestId(`settings-nav-${section}`));
  return { h: harness, qa, user };
}

const calls = (h: AppHarness, method: string) => h.fake.calls.filter((c) => c.method === method);

describe("Rede: DNS", () => {
  it("shows the status and, on enable, the UAC notice with progress, then the NRPT mode", async () => {
    const { user } = await open("network");
    const card = await screen.findByTestId("setting-dns");
    expect(await within(card).findByTestId("dns-status")).toHaveTextContent("Stopped");
    // The CLI hint of the button is on the wrapper (and in its tooltip).
    expect(card.querySelector('[data-forge-cli="forge dns enable"]')).not.toBeNull();
    await user.click(within(card).getByRole("button", { name: "Enable DNS" }));
    const running = await within(card).findByTestId("dns-enable-status");
    expect(running).toHaveAttribute("data-phase", "running");
    expect(within(running).getByRole("progressbar")).toBeInTheDocument();
    expect(within(card).getByTestId("dns-enable-status-uac")).toHaveTextContent(/UAC/);
    await waitFor(() => {
      expect(within(card).getByTestId("dns-mode")).toHaveTextContent("NRPT rule");
    });
    expect(within(card).getByTestId("dns-listen")).toHaveTextContent("127.0.0.1:53");
    expect(within(card).getByRole("button", { name: "Disable DNS" })).toBeInTheDocument();
  });

  it("shows the hosts fallback with its reason", async () => {
    const qa = createNetworkSecurityQa({ fallback: true });
    qa.state.dns.enabled = true;
    await open("network", { qa });
    const card = await screen.findByTestId("setting-dns");
    expect(await within(card).findByTestId("dns-mode")).toHaveTextContent("hosts file fallback");
    expect(within(card).getByTestId("dns-reason")).toHaveTextContent(/Group Policy/);
  });

  it("explains needs_elevation with the cause and a retry that works", async () => {
    const qa = createNetworkSecurityQa({ elevation: "refused" });
    const { h, user } = await open("network", { qa });
    const card = await screen.findByTestId("setting-dns");
    await user.click(await within(card).findByRole("button", { name: "Enable DNS" }));
    const failed = await within(card).findByTestId("dns-enable-status");
    await waitFor(() => {
      expect(failed).toHaveAttribute("data-phase", "failed");
    });
    expect(within(failed).getByRole("alert")).toHaveTextContent(/administrator/i);
    qa.state.elevation = "granted";
    await user.click(within(failed).getByRole("button", { name: "Try again" }));
    await waitFor(() => {
      expect(within(card).getByTestId("dns-mode")).toHaveTextContent("NRPT rule");
    });
    expect(calls(h, "net.dns_enable")).toHaveLength(2);
  });

  it("tests a host: resolved vs expected, and the exact command", async () => {
    const { h, user } = await open("network");
    const card = await screen.findByTestId("setting-dns");
    expect(within(card).getByText("forge dns test meusite.test")).toBeInTheDocument();
    await user.click(within(card).getByRole("button", { name: "Test host" }));
    const result = await within(card).findByTestId("dns-test-result");
    expect(result).toHaveAttribute("data-ok", "false");
    expect(within(result).getByTestId("dns-test-resolved")).toHaveTextContent("203.0.113.9");
    expect(within(result).getByTestId("dns-test-expected")).toHaveTextContent("127.0.0.1");
    expect(calls(h, "net.dns_test")[0]?.params).toEqual({ host: "meusite.test" });
  });

  it("shows the placeholder command while the host is empty", async () => {
    const { user } = await open("network");
    const card = await screen.findByTestId("setting-dns");
    await user.clear(within(card).getByRole("textbox", { name: "Host to test" }));
    expect(within(card).getByText("forge dns test <host>")).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Test host" })).toBeDisabled();
  });
});

describe("Rede: TLD", () => {
  it("asks first; cancel puts the old TLD back and calls nothing", async () => {
    const { h, user } = await open("network");
    const row = await screen.findByTestId("setting-tld");
    const field = await within(row).findByRole("textbox", { name: "Local TLD" });
    expect(field).toHaveValue(".test");
    await user.clear(field);
    await user.type(field, ".dev");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    const dialog = await screen.findByTestId("tld-dialog");
    expect(dialog).toHaveTextContent(/reissues all certificates/);
    expect(within(dialog).getByText("forge config set tld .dev")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.queryByTestId("tld-dialog")).toBeNull();
    });
    expect(within(row).getByRole("textbox", { name: "Local TLD" })).toHaveValue(".test");
    expect(calls(h, "system.config_set")).toHaveLength(0);
    expect(calls(h, "tls.ca_rotate")).toHaveLength(0);
  });

  it("refuses a malformed TLD without asking", async () => {
    const { h, user } = await open("network");
    const row = await screen.findByTestId("setting-tld");
    const field = await within(row).findByRole("textbox", { name: "Local TLD" });
    await user.clear(field);
    await user.type(field, "dev");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    expect(screen.queryByTestId("tld-dialog")).toBeNull();
    expect(within(row).getByText(/Use a dot/)).toBeInTheDocument();
    expect(calls(h, "system.config_set")).toHaveLength(0);
  });

  it("saves on confirm and offers the rotation the daemon asked for", async () => {
    const { h, qa, user } = await open("network");
    const row = await screen.findByTestId("setting-tld");
    const field = await within(row).findByRole("textbox", { name: "Local TLD" });
    await user.clear(field);
    await user.type(field, ".dev");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    await user.click(
      within(await screen.findByTestId("tld-dialog")).getByRole("button", { name: "Change TLD" }),
    );
    await waitFor(() => {
      expect(calls(h, "system.config_set")).toHaveLength(1);
    });
    expect(calls(h, "system.config_set")[0]?.params).toEqual({ key: "tld", value: ".dev" });
    const requires = await within(row).findByTestId("tld-requires");
    expect(calls(h, "tls.ca_rotate")).toHaveLength(0);
    await user.click(within(requires).getByRole("button", { name: "Rotate CA" }));
    // The destructive modal comes first; nothing runs until it is confirmed.
    const modal = await screen.findByTestId("rotate-dialog");
    expect(calls(h, "tls.ca_rotate")).toHaveLength(0);
    await user.click(within(modal).getByRole("button", { name: "Rotate now" }));
    await waitFor(() => {
      expect(calls(h, "tls.ca_rotate")).toHaveLength(1);
    });
    expect(calls(h, "tls.ca_rotate")[0]?.params).toEqual({ confirm: true });
    await waitFor(() => {
      expect(qa.state.ca.thumbprint_sha256).toBe("NEW9988776655");
    });
  });

  it("shows the daemon's refusal and keeps the old TLD", async () => {
    const { user } = await open("network", {
      handlers: {
        "system.config_set": () => {
          fakeError("config_invalid", "tld .dev is reserved");
        },
      },
    });
    const row = await screen.findByTestId("setting-tld");
    const field = await within(row).findByRole("textbox", { name: "Local TLD" });
    await user.clear(field);
    await user.type(field, ".dev");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    await user.click(
      within(await screen.findByTestId("tld-dialog")).getByRole("button", { name: "Change TLD" }),
    );
    expect(await within(row).findByTestId("tld-error")).toBeInTheDocument();
    expect(within(row).getByRole("textbox", { name: "Local TLD" })).toHaveValue(".test");
  });
});

describe("Rede: ports, port map and LAN", () => {
  it("lists the ranges and saves one as the JSON forgebox.toml keeps", async () => {
    const { h, user } = await open("network");
    const row = await screen.findByTestId("range-web-alt");
    const field = within(row).getByRole("textbox", { name: "web-alt" });
    expect(field).toHaveValue("8080, 8443");
    expect(within(screen.getByTestId("range-databases")).getByRole("textbox")).toHaveValue(
      "3306-3399, 5432-5499",
    );
    await user.clear(field);
    await user.type(field, "8000-8010, 8443");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(calls(h, "system.config_set")).toHaveLength(1);
    });
    expect(calls(h, "system.config_set")[0]?.params).toEqual({
      key: "ports.ranges.web-alt",
      value: ["8000-8010", 8443],
    });
    expect(await screen.findByTestId("range-web-alt-saved")).toBeInTheDocument();
  });

  it("refuses an invalid range without calling the daemon", async () => {
    const { h, user } = await open("network");
    const row = await screen.findByTestId("range-web");
    const field = within(row).getByRole("textbox", { name: "web" });
    await user.clear(field);
    await user.type(field, "99999");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    expect(within(row).getByText(/Use ports from 1 to 65535/)).toBeInTheDocument();
    expect(calls(h, "system.config_set")).toHaveLength(0);
  });

  it("checks a single port live with the PortField", async () => {
    const { h, user } = await open("network", {
      handlers: { "net.port_check": () => ({ free: true }) },
    });
    const row = await screen.findByTestId("setting-ports");
    await user.type(within(row).getByRole("textbox", { name: "Check a port" }), "8080");
    await waitFor(() => {
      expect(calls(h, "net.port_check").length).toBeGreaterThan(0);
    });
    expect(await within(row).findByText(/free/i)).toBeInTheDocument();
  });

  it("shows the read-only port map with the external conflict", async () => {
    await open("network");
    const map = await screen.findByTestId("port-map");
    expect(within(map).getByTestId("port-80")).toHaveTextContent("nginx");
    expect(within(map).getByTestId("port-3306")).toHaveTextContent("Reserved");
    const external = await screen.findByTestId("port-external-443");
    expect(external).toHaveTextContent("httpd.exe");
    expect(external).toHaveTextContent("pid 4412");
  });

  it("shows LAN disabled as coming soon", async () => {
    await open("network");
    const row = await screen.findByTestId("setting-lan");
    expect(within(row).getByText("Coming soon")).toBeInTheDocument();
    expect(within(row).getByRole("switch")).toBeDisabled();
  });
});

describe("Segurança: CA", () => {
  it("shows the status and the trust per store", async () => {
    await open("security");
    const status = await screen.findByTestId("ca-status");
    expect(status).toHaveTextContent("Forgebox Local CA");
    expect(status).toHaveTextContent("AB12CD34EF56");
    expect(status).toHaveTextContent("Current user");
    expect(status).toHaveTextContent("Whole machine: no");
  });

  it("installs for the user, then for the machine with the UAC notice and its command", async () => {
    const { h, user } = await open("security");
    const card = await screen.findByTestId("setting-ca");
    expect(within(card).getByTestId("ca-install-note")).toHaveTextContent(/security dialog/);
    expect(within(card).getByText("forge tls ca install --scope user")).toBeInTheDocument();
    await user.click(within(card).getByRole("radio", { name: "Whole machine" }));
    expect(within(card).getByTestId("ca-install-note")).toHaveTextContent(/UAC/);
    expect(within(card).getByText("forge tls ca install --scope machine")).toBeInTheDocument();
    await user.click(within(card).getByRole("button", { name: "Install CA" }));
    expect(await within(card).findByTestId("ca-install-status-uac")).toBeInTheDocument();
    await waitFor(() => {
      expect(within(card).getByTestId("ca-install-status")).toHaveAttribute("data-phase", "done");
    });
    expect(calls(h, "tls.ca_install")[0]?.params).toEqual({ scope: "local_machine" });
    await waitFor(() => {
      expect(within(card).getByTestId("ca-status")).toHaveTextContent("Whole machine");
      expect(within(card).getByTestId("ca-status")).not.toHaveTextContent("Whole machine: no");
    });
  });

  it("shows needs_elevation of a machine install with a retry", async () => {
    const qa = createNetworkSecurityQa({ elevation: "refused" });
    const { user } = await open("security", { qa });
    const card = await screen.findByTestId("setting-ca");
    await user.click(within(card).getByRole("radio", { name: "Whole machine" }));
    await user.click(within(card).getByRole("button", { name: "Install CA" }));
    const failed = await within(card).findByTestId("ca-install-status");
    await waitFor(() => {
      expect(failed).toHaveAttribute("data-phase", "failed");
    });
    expect(within(failed).getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("exports through the save dialog with the exact command", async () => {
    const { h, user } = await open("security");
    const card = await screen.findByTestId("setting-ca");
    expect(within(card).getByText("forge tls ca export -o ca.crt")).toBeInTheDocument();
    await user.click(within(card).getByRole("button", { name: "Export CA…" }));
    await waitFor(() => {
      expect(native.saveCaCertificate).toHaveBeenCalledWith("forgebox-ca.crt", QA_PEM);
    });
    expect(calls(h, "tls.ca_export")).toHaveLength(1);
  });

  it("does not report a cancelled save dialog as saved", async () => {
    native.saveCaCertificate.mockResolvedValueOnce({ cancelled: true });
    const { user } = await open("security");
    const card = await screen.findByTestId("setting-ca");
    await user.click(within(card).getByRole("button", { name: "Export CA…" }));
    await waitFor(() => {
      expect(native.saveCaCertificate).toHaveBeenCalled();
    });
    expect(screen.queryByText(/CA saved to/)).toBeNull();
  });

  it("rotates only after the destructive modal and shows the job", async () => {
    const { h, user } = await open("security");
    const card = await screen.findByTestId("setting-ca");
    expect(within(card).getAllByText("forge tls ca rotate").length).toBeGreaterThan(0);
    await user.click(within(card).getByTestId("ca-rotate"));
    const modal = await screen.findByTestId("rotate-dialog");
    expect(modal).toHaveTextContent(/reissues every site certificate/);
    expect(calls(h, "tls.ca_rotate")).toHaveLength(0);
    // Cancel: nothing runs.
    await user.click(within(modal).getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.queryByTestId("rotate-dialog")).toBeNull();
    });
    expect(calls(h, "tls.ca_rotate")).toHaveLength(0);
    await user.click(within(card).getByTestId("ca-rotate"));
    await user.click(
      within(await screen.findByTestId("rotate-dialog")).getByRole("button", {
        name: "Rotate now",
      }),
    );
    const status = await within(card).findByTestId("ca-rotate-status");
    expect(status).toHaveAttribute("data-phase", "running");
    expect(calls(h, "tls.ca_rotate")[0]?.params).toEqual({ confirm: true });
    await waitFor(() => {
      expect(within(card).getByTestId("ca-rotate-status")).toHaveAttribute("data-phase", "done");
    });
  });

  it("uninstalls through tls.ca_uninstall and disables a store that does not trust it", async () => {
    const { h, user } = await open("security");
    const card = await screen.findByTestId("setting-ca");
    expect(within(card).getByText("forge tls ca uninstall")).toBeInTheDocument();
    expect(within(card).getByTestId("ca-uninstall-machine")).toBeDisabled();
    await user.click(within(card).getByTestId("ca-uninstall-user"));
    await waitFor(() => {
      expect(calls(h, "tls.ca_uninstall")).toHaveLength(1);
    });
    expect(calls(h, "tls.ca_uninstall")[0]?.params).toEqual({ scope: "current_user" });
    await waitFor(() => {
      expect(within(card).getByTestId("ca-uninstall-user-status")).toHaveAttribute(
        "data-phase",
        "done",
      );
    });
  });
});

describe("Segurança: certificates and helper log", () => {
  it("lists certificates with their state and renews one or all", async () => {
    const { h, user } = await open("security");
    const list = await screen.findByTestId("certs-list");
    expect(within(list).getByTestId("cert-loja.test")).toHaveAttribute("data-state", "expiring");
    expect(within(list).getByTestId("cert-blog.test")).toHaveAttribute("data-state", "ok");
    expect(within(list).getByTestId("cert-velho.test")).toHaveAttribute("data-state", "expired");
    expect(within(list).getByTestId("cert-antigo.test")).toHaveAttribute("data-state", "untrusted");
    await user.click(within(list).getByTestId("cert-renew-velho.test"));
    await waitFor(() => {
      expect(calls(h, "tls.renew")).toHaveLength(1);
    });
    expect(calls(h, "tls.renew")[0]?.params).toEqual({ host: "velho.test" });
    await waitFor(() => {
      expect(screen.getByTestId("cert-velho.test")).toHaveAttribute("data-state", "ok");
    });
    expect(screen.getByText("forge tls renew --all")).toBeInTheDocument();
    await user.click(screen.getByTestId("cert-renew-all"));
    await waitFor(() => {
      expect(calls(h, "tls.renew")).toHaveLength(2);
    });
    expect(calls(h, "tls.renew")[1]?.params).toEqual({ all: true });
  });

  it("tails the helper log through fs.tail", async () => {
    const { h } = await open("security");
    const log = await screen.findByTestId("helper-log");
    expect(log).toHaveTextContent("dns.nrpt_apply ok NRPT rule applied for .test");
    expect(log).toHaveTextContent("trust.install_machine_root failed");
    expect(calls(h, "fs.tail")[0]?.params).toEqual({ path: "logs/helper.log", lines: 100 });
  });

  it("says why the helper log is unavailable below the Admin role", async () => {
    const qa = createNetworkSecurityQa();
    harness = await renderApp({
      path: "/settings",
      extra: { handlers: { ...settingsHandlers().handlers, ...qa.handlers } },
    });
    harness.fake.setHello({ role: "operator" });
    await harness.settle();
    await userEvent.setup().click(await screen.findByTestId("settings-nav-security"));
    expect(await screen.findByTestId("helper-log-denied")).toBeInTheDocument();
    expect(screen.queryByTestId("helper-log")).toBeNull();
  });
});
