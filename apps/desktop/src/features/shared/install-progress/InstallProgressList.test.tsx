// Acceptance of M2-E2-T18 (criterion 2): the progress list with a known total (determinate bar,
// bytes, speed, ETA), an unknown total (indeterminate bar + step text), cancel -> cancelled,
// the morph to a check and a failed row with its cause.
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18next from "i18next";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { startJobAction } from "../../../actions/registry";
import { installTools } from "../../../actions/tools";
import { fakeJob, type FakeScenario } from "../../../ipc/fakeBridge";
import { ForgeStateProvider } from "../../../state";
import { startHarness, type Harness } from "../../../state/testFixtures";
import { InstallProgressList } from "./InstallProgressList";
import { barValue } from "./model";

beforeAll(async () => {
  await i18next.loadNamespaces(["catalog", "errors"]);
});

let harness: Harness | null = null;
afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

const MB = 1024 * 1024;

/** Starts the install the scenario scripts and renders the list for its job. */
async function mountInstall(scenario: FakeScenario) {
  harness = await startHarness({ scenario: { notImplemented: [], ...scenario } });
  const h = harness;
  const handle = await startJobAction(h.runtime.queue, installTools, {
    specs: ["php@8.4.27"],
    variant: "nts",
  });
  render(
    <ForgeStateProvider layer={h.layer}>
      <InstallProgressList items={[{ key: "a", jobId: handle.job.id, label: "PHP 8.4.27" }]} />
    </ForgeStateProvider>,
  );
  // `done` of a cancelled or failed job rejects: the row shows it, not this promise.
  handle.done.catch(() => undefined);
  return h;
}

describe("barValue", () => {
  it("uses pct, else bytes over a known total, else null", () => {
    expect(barValue({ pct: 40 })).toBe(40);
    expect(barValue({ bytes_done: 5, bytes_total: 20 })).toBe(25);
    expect(barValue({ bytes_done: 5 })).toBeNull();
    expect(barValue({ bytes_done: 5, bytes_total: 0 })).toBeNull();
    expect(barValue({ message: "extracting" })).toBeNull();
  });

  it("never reaches 100 while the job runs (the check is for done)", () => {
    expect(barValue({ pct: 100 })).toBe(99);
    expect(barValue({ bytes_done: 20, bytes_total: 20 })).toBe(99);
  });
});

describe("<InstallProgressList>", () => {
  it("renders a determinate bar with bytes, speed and ETA when the total is known", async () => {
    await mountInstall({
      handlers: {
        "tools.install": () =>
          fakeJob({
            hold: true,
            steps: [
              {
                message: "downloading",
                pct: 25,
                bytes_done: 8 * MB,
                bytes_total: 32 * MB,
                speed_bps: 4 * MB,
                eta_secs: 6,
              },
            ],
          }),
      },
    });
    const bar = await screen.findByRole("progressbar", { name: /Installing PHP 8.4.27/ });
    await waitFor(() => {
      expect(bar).toHaveAttribute("aria-valuenow", "25");
    });
    const row = document.querySelector("[data-install-job]");
    expect(row?.querySelector('[data-state="loading"]')).not.toBeNull();
    expect(row?.textContent).toContain("8 MB of 32 MB");
    expect(row?.textContent).toContain("downloading");
    expect(row?.textContent).toMatch(/4 MB\/s/);
    expect(row?.textContent).toMatch(/6s/);
  });

  it("renders an indeterminate bar with the step text when the total is unknown", async () => {
    await mountInstall({
      handlers: {
        "tools.install": () => fakeJob({ hold: true, steps: [{ message: "verifying sha256" }] }),
      },
    });
    const bar = await screen.findByRole("progressbar", { name: /Installing PHP 8.4.27/ });
    await waitFor(() => {
      expect(screen.getByText("verifying sha256")).toBeInTheDocument();
    });
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(document.querySelector('[data-state="indeterminate"]')).not.toBeNull();
  });

  it("cancels through jobs.cancel and shows the cancelled state", async () => {
    const h = await mountInstall({
      handlers: {
        "tools.install": () =>
          fakeJob({ hold: true, steps: [{ message: "downloading", pct: 10 }] }),
      },
    });
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(document.querySelector('[data-state="cancelled"]')).not.toBeNull();
    });
    expect(h.count("jobs.cancel")).toBe(1);
    expect(screen.getByText(/Install cancelled/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("morphs to a check when the job is done", async () => {
    await mountInstall({
      handlers: {
        "tools.install": () =>
          fakeJob({ stepMs: 5, steps: [{ pct: 50 }], outcome: { done: { installed: [] } } }),
      },
    });
    await waitFor(() => {
      expect(document.querySelector('[data-state="done"]')).not.toBeNull();
    });
    expect(document.querySelector('[data-part="check"]')).not.toBeNull();
    expect(screen.getByText("Installed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("shows the cause of a failed install", async () => {
    await mountInstall({
      handlers: {
        "tools.install": () =>
          fakeJob({
            stepMs: 5,
            outcome: { failed: { code: "network", message: "download failed" } },
          }),
      },
    });
    await act(async () => {
      await Promise.resolve();
    });
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(document.querySelector('[data-state="failed"]')).not.toBeNull();
  });
});
