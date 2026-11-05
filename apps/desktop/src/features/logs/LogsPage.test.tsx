// The /logs screen end to end over the fake bridge: picker, merged stream, deep link, Ctrl+L.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { logsHandlers } from "../../../qa/fixtures/scenarios/logs";
import { renderApp, type AppHarness } from "../../app/testApp";

let harness: AppHarness | null = null;

beforeEach(() => {
  // jsdom has no layout: give every element the viewport the virtualizer reads.
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
});

afterEach(async () => {
  delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  await harness?.layer.dispose();
  harness = null;
});

async function open(path = "/logs") {
  harness = await renderApp({ path, extra: { handlers: logsHandlers() } });
  return harness;
}

describe("Logs screen", () => {
  it("lists the sources by group and merges the ones picked, by time", async () => {
    const user = userEvent.setup();
    const h = await open();
    const picker = await screen.findByRole("group", { name: "Sources" });
    expect(
      [...picker.querySelectorAll("fieldset")].map((f) => f.getAttribute("data-group")),
    ).toEqual(["daemon", "nginx", "mysql", "php", "loja.test"]);
    expect(await screen.findByText("Pick a source to see its lines.")).toBeInTheDocument();

    await user.click(within(picker).getByRole("checkbox", { name: "nginx · error" }));
    await user.click(within(picker).getByRole("checkbox", { name: "mysql" }));
    const log = await screen.findByRole("log");
    await waitFor(() => {
      expect(h.fake.openStreams()).toBeGreaterThan(2);
    });
    const line = (target: string, text: string, ts: string) => {
      h.fake.emitLog(target, { ts, source: "stdout", level: null, text });
    };
    act(() => {
      line("mysql/default", "second", "2026-01-01T10:00:02Z");
      line("nginx.error", "first", "2026-01-01T10:00:01Z");
      line("nginx.error", "third", "2026-01-01T10:00:03Z");
      h.fake.flush();
    });
    expect(screen.getByText("2 sources")).toBeInTheDocument();
    await waitFor(() => {
      const texts = [...log.querySelectorAll("[data-row]")].map((row) => row.textContent);
      expect(texts).toHaveLength(3);
      // Several sources: each row says where it comes from; the order is by ts, not arrival.
      expect(texts[0]).toContain("first");
      expect(texts[1]).toContain("second");
      expect(texts[2]).toContain("third");
    });
  });

  it("opens with the source of ?target= and shows the CLI line for it", async () => {
    await open("/logs?target=nginx.error");
    expect(await screen.findByRole("log", { name: "Log lines of nginx.error" })).toBeVisible();
    expect(screen.getByText("1 source")).toBeInTheDocument();
    expect(document.querySelector("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge logs nginx.error -f",
    );
  });

  it("Ctrl+L focuses the screen, also when already on it", async () => {
    const user = userEvent.setup();
    await open();
    const page = await screen.findByTestId("page-logs");
    await waitFor(() => {
      expect(page).toHaveFocus();
    });
    await user.click(await screen.findByRole("checkbox", { name: "Forgebox daemon" }));
    await user.keyboard("{Control>}l{/Control}");
    await waitFor(() => {
      expect(page).toHaveFocus();
    });
  });

  it("R-16: a daemon without logs.targets says so, never an endless skeleton", async () => {
    harness = await renderApp({
      path: "/logs",
      extra: { handlers: logsHandlers(), notImplemented: ["logs.targets"] },
    });
    expect(await screen.findByText("Not available yet in this version of Forgebox.")).toBeVisible();
    expect(document.querySelector("[data-skeleton]")).toBeNull();
  });

  it("R-16: with the daemon stopped the picker offers to start it", async () => {
    harness = await renderApp({ path: "/logs", scenario: "daemon-down" });
    expect(await screen.findByText("No connection to the daemon")).toBeVisible();
    expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
    expect(document.querySelector("[data-skeleton]")).toBeNull();
  });

  it("R-28: with no source nothing is enabled, and each disabled button says why", async () => {
    await open();
    await screen.findByText("Pick a source to see its lines.");
    for (const name of ["Pause", "Copy", "Load earlier"]) {
      const button = screen.getByRole("button", { name });
      expect(button, name).toBeDisabled();
      const reason = button.title || button.parentElement?.title;
      expect(reason, name).toBe("Pick a source to see its lines.");
    }
  });
});
