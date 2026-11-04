// Acceptance of M2-E2-T19: the four-step wizard over the fake bridge (call order, interaction
// count, a refused UAC, a failed download and its re-plan, remote and portable, resume).
import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createOnboardingQa,
  type OnboardingQaOptions,
} from "../../../qa/fixtures/scenarios/onboarding";
import { siteHandlers } from "../../../qa/fixtures/scenarios";
import { renderApp, type AppHarness } from "../../app/testApp";
import { populatedData } from "../../state/testFixtures";
import { resetIconCache } from "../catalog/CatalogIcon";
import { createMemoryNative, setOnboardingNativeForTest, type MemoryNativeOptions } from "./native";

vi.mock("../../ipc/native", () => ({
  copyText: () => Promise.resolve(true),
  openUrl: () => Promise.resolve(true),
  pickPath: () => Promise.resolve(null),
  onPathsDropped: () => Promise.resolve(() => undefined),
}));

let harness: AppHarness | null = null;
beforeEach(() => {
  resetIconCache();
});
afterEach(async () => {
  cleanup();
  setOnboardingNativeForTest(null);
  await harness?.layer.dispose();
  harness = null;
});

const WAIT = { timeout: 10_000 };

async function open(
  qa: OnboardingQaOptions = {},
  nativeOptions: MemoryNativeOptions = {},
  extra: Parameters<typeof renderApp>[0] = {},
) {
  const native = createMemoryNative(nativeOptions);
  setOnboardingNativeForTest(native);
  const data = populatedData();
  const qaData = { instances: [], sites: [], tools: [], daemon: data.daemon };
  const scenario = createOnboardingQa(qaData, qa);
  scenario.handlers = { ...siteHandlers(qaData), ...scenario.handlers };
  harness = await renderApp({
    path: "/onboarding",
    data,
    ...extra,
    extra: { notImplemented: [], handlers: scenario.handlers, ...extra.extra },
  });
  return { h: harness, native, scenario };
}

/** A user whose every click, keystroke and selection counts as one interaction. */
function countingUser() {
  const user = userEvent.setup();
  const counter = { n: 0 };
  return {
    counter,
    click: async (element: Element) => {
      counter.n += 1;
      await user.click(element);
    },
    key: async (keys: string) => {
      counter.n += 1;
      await user.keyboard(keys);
    },
    raw: user,
  };
}

const methods = (h: AppHarness) => h.fake.calls.map((call) => call.method);

/** The methods in the order they were first called. */
function firstOrder(all: readonly string[], wanted: readonly string[]): string[] {
  const seen: string[] = [];
  for (const method of all) {
    if (wanted.includes(method) && !seen.includes(method)) seen.push(method);
  }
  return seen;
}

const step = (id: string) => document.querySelector<HTMLElement>(`[data-run-step="${id}"]`);

/** A step row that must exist. */
function row(id: string): HTMLElement {
  const found = step(id);
  if (found === null) throw new Error(`no run step ${id}`);
  return found;
}

async function throughToRun(user: ReturnType<typeof countingUser>, skipRoot = false) {
  if (!skipRoot) {
    await user.click(await screen.findByRole("button", { name: "Continue" }, WAIT));
  }
  await user.click(await screen.findByRole("radio", { name: /Laravel/ }, WAIT));
  await screen.findByTestId("plan-total", undefined, WAIT);
  await user.click(screen.getByRole("button", { name: "Continue" }));
  await user.click(await screen.findByRole("button", { name: "Install and set up" }));
}

describe("Onboarding wizard", () => {
  it("completes the Laravel path in at most 12 interactions, calling in order", async () => {
    const { h, native } = await open();
    const user = countingUser();
    await screen.findByRole("heading", { level: 2, name: "Where should Forgebox live?" }, WAIT);
    await throughToRun(user);
    await waitFor(() => {
      expect(step("ca")?.dataset.status).toBe("done");
    }, WAIT);
    expect(step("install")?.dataset.status).toBe("done");
    expect(step("path")?.dataset.status).toBe("done");
    expect(step("dns")?.dataset.status).toBe("done");
    // The three downloads, one row each, with the DNS result in plain words.
    expect(document.querySelectorAll("[data-install-job]")).toHaveLength(3);
    expect(screen.getByText(/Wildcards such as \*\.test resolve/)).toBeInTheDocument();

    await user.click(await screen.findByRole("button", { name: "Finish" }));
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/dashboard");
    }, WAIT);

    expect(user.counter.n).toBeLessThanOrEqual(12);
    expect(
      firstOrder(methods(h), [
        "system.config_set",
        "sites.park",
        "tools.plan",
        "tools.install",
        "shell.path_add",
        "net.dns_enable",
        "tls.ca_install",
      ]),
    ).toEqual([
      "sites.park",
      "tools.plan",
      "tools.install",
      "shell.path_add",
      "net.dns_enable",
      "tls.ca_install",
    ]);
    expect(native.chosen).toBe("C:/Forgebox");
    expect((await native.get("local")).done).toBe(true);
  });

  it("sets a changed TLD before parking, and refuses a name that is not for local use", async () => {
    const { h } = await open();
    const user = countingUser();
    const tld = await screen.findByRole("textbox", { name: "Local domain (TLD)" }, WAIT);
    await user.raw.clear(tld);
    await user.raw.type(tld, "dev.com");
    expect(await screen.findByText(/not reserved for local use/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    await user.raw.clear(tld);
    await user.raw.type(tld, "Dev.Test");
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "What will you build?" }, WAIT);
    expect(firstOrder(methods(h), ["system.config_set", "sites.park"])).toEqual([
      "system.config_set",
      "sites.park",
    ]);
    const set = h.fake.calls.find((call) => call.method === "system.config_set");
    expect(set?.params).toEqual({ key: "tld", value: ".dev.test" });
    const park = h.fake.calls.find((call) => call.method === "sites.park");
    expect(park?.params).toEqual({ dir: "C:/Forgebox/www", tld: ".dev.test" });
  });

  it("skips a refused UAC with its command and goes on to the CA", async () => {
    const { h } = await open({ uac: true });
    const user = countingUser();
    await throughToRun(user);
    await waitFor(() => {
      expect(step("ca")?.dataset.status).toBe("done");
    }, WAIT);
    const dns = row("dns");
    expect(dns.dataset.status).toBe("skipped");
    expect(dns.dataset.skip).toBe("elevation");
    expect(dns.querySelector('[data-forge-cli="forge dns enable"]')).not.toBeNull();
    expect(within(dns).getByRole("button", { name: "Try again" })).toBeEnabled();
    expect(step("path")?.dataset.status).toBe("done");
    expect(methods(h)).toContain("tls.ca_install");
  });

  it("explains the hosts fallback and that wildcards do not resolve there", async () => {
    const { h } = await open({ hosts: true });
    const user = countingUser();
    await throughToRun(user);
    await waitFor(() => {
      expect(step("dns")?.dataset.status).toBe("done");
    }, WAIT);
    const text = step("dns")?.querySelector("[data-result=dns]")?.textContent ?? "";
    expect(text).toContain("hosts file");
    expect(text).toContain("Group Policy NRPT table");
    expect(text).toContain("wildcards (*.test) do not resolve");
    expect(methods(h)).toContain("net.dns_enable");
  });

  it("shows why a download failed and re-plans only the missing tools on retry", async () => {
    const { h, scenario } = await open({ fail: true });
    const user = countingUser();
    await throughToRun(user);
    await waitFor(() => {
      expect(step("install")?.dataset.status).toBe("failed");
    }, WAIT);
    // The other steps went on while the download was failed.
    await waitFor(() => {
      expect(step("ca")?.dataset.status).toBe("done");
    }, WAIT);
    const install = row("install");
    expect(install.querySelector('[data-error-code="verification_failed"]')).not.toBeNull();
    expect([...scenario.installed].sort()).toEqual(["php@8.4.27", "redis@7.4.1"]);
    const installsBefore = h.fake.calls.filter((call) => call.method === "tools.install").length;
    const plansBefore = methods(h).filter((method) => method === "tools.plan").length;

    const retry = within(install)
      .getAllByRole("button", { name: /Try again|Retry/ })
      .at(0);
    if (retry === undefined) throw new Error("no retry button");
    await user.click(retry);
    await waitFor(() => {
      expect(step("install")?.dataset.status).toBe("done");
    }, WAIT);
    const installs = h.fake.calls.filter((call) => call.method === "tools.install");
    expect(installs).toHaveLength(installsBefore + 1);
    expect(installs.at(-1)?.params).toMatchObject({ specs: ["mysql@8.4.3"] });
    expect(methods(h).filter((method) => method === "tools.plan").length).toBeGreaterThan(
      plansBefore,
    );
    expect([...scenario.installed].sort()).toEqual(["mysql@8.4.3", "php@8.4.27", "redis@7.4.1"]);
  });

  it("re-plans when a version changes and lists the new total", async () => {
    const { h } = await open();
    const user = countingUser();
    await user.click(await screen.findByRole("button", { name: "Continue" }, WAIT));
    await user.click(await screen.findByRole("radio", { name: /Laravel/ }, WAIT));
    await screen.findByTestId("plan-total", undefined, WAIT);
    await user.click(screen.getByRole("button", { name: "Adjust versions" }));
    const picker = await screen.findByRole("combobox", { name: "php version" }, WAIT);
    await user.click(picker);
    await user.click(await screen.findByRole("option", { name: "8.3" }));
    await waitFor(() => {
      expect(
        h.fake.calls.some(
          (call) =>
            call.method === "tools.plan" &&
            (call.params as { specs?: string[] }).specs?.includes("php@8.3") === true,
        ),
      ).toBe(true);
    }, WAIT);
    await screen.findByText("php 8.3.40", undefined, WAIT);
  });

  it("offers Vazio and installs nothing", async () => {
    const { h } = await open();
    const user = countingUser();
    await user.click(await screen.findByRole("button", { name: "Continue" }, WAIT));
    await user.click(await screen.findByRole("radio", { name: /Empty/ }, WAIT));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await user.click(await screen.findByRole("button", { name: "Install and set up" }));
    await waitFor(() => {
      expect(step("ca")?.dataset.status).toBe("done");
    }, WAIT);
    expect(step("install")?.dataset.skip).toBe("nothing");
    expect(methods(h)).not.toContain("tools.install");
  });

  it("hides step 1 for a remote daemon, with the reason, and the PATH and startup toggles", async () => {
    await open({}, { local: false }, { scenario: "remote" });
    await screen.findByRole("heading", { level: 2, name: "What will you build?" }, WAIT);
    expect(screen.getByTestId("onboarding-hidden-note")).toHaveAttribute("data-reason", "remote");
    expect(screen.queryByText("Folders")).toBeNull();
    const user = countingUser();
    await user.click(await screen.findByRole("radio", { name: /Empty/ }, WAIT));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" });
    expect(screen.queryByRole("switch", { name: "Add Forgebox to PATH" })).toBeNull();
    expect(screen.queryByRole("switch", { name: "Start with Windows" })).toBeNull();
    expect(screen.getByRole("switch", { name: "Trust the local CA" })).toBeInTheDocument();
  });

  it("hides step 1 and the PATH and startup toggles in portable mode", async () => {
    await open(
      {},
      {
        source: "portable",
        autostart: { available: false, enabled: false, reason: "portable" },
      },
    );
    await screen.findByRole("heading", { level: 2, name: "What will you build?" }, WAIT);
    expect(screen.getByTestId("onboarding-hidden-note")).toHaveAttribute("data-reason", "portable");
    const user = countingUser();
    await user.click(await screen.findByRole("radio", { name: /Empty/ }, WAIT));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" });
    expect(screen.queryByRole("switch", { name: "Add Forgebox to PATH" })).toBeNull();
    expect(screen.queryByRole("switch", { name: "Start with Windows" })).toBeNull();
  });

  it("shows the parity sentence on, and the single UAC and the Windows dialog explained", async () => {
    await open();
    const user = countingUser();
    await user.click(await screen.findByRole("button", { name: "Continue" }, WAIT));
    await user.click(await screen.findByRole("radio", { name: /Empty/ }, WAIT));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" });
    expect(screen.getByRole("switch", { name: /Linux parity mode/ })).toBeChecked();
    expect(
      screen.getByText(/Upper\/lower-case mistakes show up here, not in production/),
    ).toBeVisible();
    expect(screen.getByText(/one administrator confirmation \(UAC\)/)).toBeVisible();
    expect(screen.getByText(/Windows will show a dialog/)).toBeVisible();
    expect(screen.getByRole("switch", { name: "Add Forgebox to PATH" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Start with Windows" })).not.toBeChecked();
  });

  it("closing at step 3 and opening again resumes with the previous choices", async () => {
    const first = await open();
    const user = countingUser();
    await user.click(await screen.findByRole("button", { name: "Continue" }, WAIT));
    await user.click(await screen.findByRole("radio", { name: /WordPress/ }, WAIT));
    await screen.findByTestId("plan-total", undefined, WAIT);
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" });
    await user.click(screen.getByRole("switch", { name: /Automatic DNS/ }));
    await user.click(screen.getByRole("switch", { name: /Linux parity mode/ }));

    // The window closes: nothing but the native store survives.
    const { native } = first;
    cleanup();
    await harness?.layer.dispose();
    harness = null;
    setOnboardingNativeForTest(native);
    const data = populatedData();
    harness = await renderApp({
      path: "/onboarding",
      data,
      extra: {
        notImplemented: [],
        handlers: createOnboardingQa(
          { instances: [], sites: [], tools: [], daemon: data.daemon },
          {},
        ).handlers,
      },
    });
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" }, WAIT);
    expect(screen.getByRole("switch", { name: /Automatic DNS/ })).not.toBeChecked();
    expect(screen.getByRole("switch", { name: /Linux parity mode/ })).not.toBeChecked();
    expect(screen.getByRole("switch", { name: "Trust the local CA" })).toBeChecked();
    // Going back shows the stack that was chosen.
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByRole("radio", { name: /WordPress/ }, WAIT)).toBeChecked();
  });

  it("turns Linux parity off with one config_set before the downloads", async () => {
    const { h } = await open();
    const user = countingUser();
    await user.click(await screen.findByRole("button", { name: "Continue" }, WAIT));
    await user.click(await screen.findByRole("radio", { name: /Empty/ }, WAIT));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await user.click(await screen.findByRole("switch", { name: /Linux parity mode/ }));
    await user.click(screen.getByRole("button", { name: "Install and set up" }));
    await waitFor(() => {
      expect(step("ca")?.dataset.status).toBe("done");
    }, WAIT);
    const sets = h.fake.calls.filter((call) => call.method === "system.config_set");
    expect(sets.map((call) => call.params)).toEqual([{ key: "case_sensitive", value: false }]);
    // The flag of the (still empty) parked folder is cleared right after, so new projects are
    // not case-sensitive by inheritance.
    expect(
      firstOrder(methods(h), ["system.config_set", "fs_parity.case_disable", "tools.plan"]),
    ).toEqual(["system.config_set", "fs_parity.case_disable"]);
    const order = methods(h);
    expect(order.indexOf("fs_parity.case_disable")).toBeGreaterThan(
      order.indexOf("system.config_set"),
    );
    expect(h.fake.calls.find((call) => call.method === "fs_parity.case_disable")?.params).toEqual({
      dir: "C:/Forgebox/www",
    });
  });

  it("keeps a non-default root, TLD and parked folder when the wizard is reopened", async () => {
    const first = await open();
    const user = countingUser();
    const root = await screen.findByRole("textbox", { name: "Root folder" }, WAIT);
    await user.raw.clear(root);
    await user.raw.type(root, "D:/Dev/Forgebox");
    const parked = screen.getByRole("textbox", { name: "Folder for your sites" });
    await user.raw.clear(parked);
    await user.raw.type(parked, "D:/Sites");
    const tld = screen.getByRole("textbox", { name: "Local domain (TLD)" });
    await user.raw.clear(tld);
    await user.raw.type(tld, "dev.test");
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await user.click(await screen.findByRole("radio", { name: /Empty/ }, WAIT));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" }, WAIT);

    const { native } = first;
    expect(native.chosen).toBe("D:/Dev/Forgebox");
    cleanup();
    await harness?.layer.dispose();
    harness = null;
    setOnboardingNativeForTest(native);
    const data = populatedData();
    harness = await renderApp({
      path: "/onboarding",
      data,
      extra: {
        notImplemented: [],
        handlers: {
          ...siteHandlers({ instances: [], sites: [], tools: [], daemon: data.daemon }),
          ...createOnboardingQa({ instances: [], sites: [], tools: [], daemon: data.daemon }, {})
            .handlers,
        },
      },
    });
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" }, WAIT);
    // Back to step 1 shows the values and Continue repeats nothing.
    const before = harness.fake.calls.length;
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    await userEvent.click(await screen.findByRole("button", { name: "Back" }, WAIT));
    expect(await screen.findByRole("textbox", { name: "Root folder" }, WAIT)).toHaveValue(
      "D:/Dev/Forgebox",
    );
    expect(screen.getByRole("textbox", { name: "Folder for your sites" })).toHaveValue("D:/Sites");
    expect(screen.getByRole("textbox", { name: "Local domain (TLD)" })).toHaveValue("dev.test");
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "What will you build?" }, WAIT);
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByRole("heading", { level: 2, name: "Permissions and parity" }, WAIT);
    const added = harness.fake.calls
      .slice(before)
      .map((call) => call.method)
      .filter((method) => method === "system.config_set" || method === "sites.park");
    expect(added).toEqual([]);
    await userEvent.click(screen.getByRole("button", { name: "Install and set up" }));
    await waitFor(() => {
      expect(step("ca")?.dataset.status).toBe("done");
    }, WAIT);
    const commands = [...document.querySelectorAll("[data-testid=commands] [data-forge-cli]")].map(
      (el) => el.getAttribute("data-forge-cli"),
    );
    expect(commands).toContain("forge park D:/Sites --tld .dev.test");
    expect(commands).toContain("forge config set tld .dev.test");
  });

  describe("resume from the app shell", () => {
    async function openAt(entry: MemoryNativeOptions["entry"]) {
      const native = createMemoryNative(entry === undefined ? {} : { entry });
      setOnboardingNativeForTest(native);
      const data = populatedData();
      const qaData = { instances: [], sites: [], tools: [], daemon: data.daemon };
      harness = await renderApp({
        path: "/dashboard",
        data,
        extra: {
          notImplemented: [],
          handlers: { ...siteHandlers(qaData), ...createOnboardingQa(qaData, {}).handlers },
        },
      });
      return harness;
    }

    it("goes back to the wizard when it was closed half way", async () => {
      const h = await openAt({ step: "options", done: false });
      await screen.findByRole("heading", { level: 2, name: "Permissions and parity" }, WAIT);
      expect(h.router.state.location.pathname).toBe("/onboarding");
    });

    it("stays on the dashboard when the onboarding is done", async () => {
      const h = await openAt({ step: "options", done: true });
      await new Promise((resolve) => setTimeout(resolve, 200));
      expect(h.router.state.location.pathname).toBe("/dashboard");
    });

    it("stays on the dashboard when it never started", async () => {
      const h = await openAt({ step: "", done: false });
      await new Promise((resolve) => setTimeout(resolve, 200));
      expect(h.router.state.location.pathname).toBe("/dashboard");
    });
  });

  it("creates the first site from a typed folder and lists every equivalent command", async () => {
    const { h } = await open();
    const user = countingUser();
    await throughToRun(user);
    const field = await screen.findByRole("textbox", { name: "Project folder" }, WAIT);
    await user.raw.type(field, "C:/Projects/Loja Nova");
    await user.click(screen.getByRole("button", { name: "Create site" }));
    expect(await screen.findByTestId("site-created", undefined, WAIT)).toHaveTextContent(
      "loja-nova.test is ready.",
    );
    expect(methods(h)).toContain("sites.add");
    const commands = [...document.querySelectorAll("[data-testid=commands] [data-forge-cli]")].map(
      (el) => el.getAttribute("data-forge-cli"),
    );
    expect(commands).toEqual([
      "forge park C:/Forgebox/www --tld .test",
      "forge tool install php@8.4.27 mysql@8.4.3 redis@7.4.1",
      "forge path add",
      "forge dns enable",
      "forge tls ca install --scope user",
    ]);
  });
});
