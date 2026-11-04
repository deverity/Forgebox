// Acceptance of M2-E3-T12: the LogViewer states (loading, empty, error, live), pause with the
// new-lines badge, the lost-lines marker, filters, "load earlier" (before_seq), PHP file links
// and open_editor (hidden when remote), access columns, the source picker and the CLI hint.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18next from "i18next";
import type { ReactElement } from "react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { fakeError, type FakeHandlers, type FakeScenario } from "../../ipc/fakeBridge";
import type * as T from "../../ipc/generated";
import { ForgeStateProvider } from "../../state";
import { startHarness, type Harness } from "../../state/testFixtures";
import { LogViewer } from "./LogViewer";
import { TargetPicker } from "./TargetPicker";

vi.mock("../../ipc/native", () => ({
  copyText: vi.fn(() => Promise.resolve(true)),
  pickPath: () => Promise.resolve(null),
}));

beforeAll(async () => {
  await i18next.loadNamespaces(["logs", "errors", "common"]);
});

let harness: Harness | null = null;
beforeEach(() => {
  // jsdom has no layout: give every element the viewport the virtualizer reads.
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get: () => 1000,
  });
});
afterEach(async () => {
  delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
  await harness?.layer.dispose();
  harness = null;
});

async function mount(ui: ReactElement, scenario: FakeScenario = {}, seed?: (h: Harness) => void) {
  harness = await startHarness({ scenario: { notImplemented: [], ...scenario } });
  const h = harness;
  seed?.(h);
  render(<ForgeStateProvider layer={h.layer}>{ui}</ForgeStateProvider>);
  return h;
}

function emit(h: Harness, target: string, texts: string[], extra: Partial<T.LogLineDto> = {}) {
  for (const text of texts) {
    h.fake.emitLog(target, { ts: null, source: "stdout", level: null, text, ...extra });
  }
}

async function flush(h: Harness) {
  await act(async () => {
    h.fake.flush();
    await new Promise((resolve) => setTimeout(resolve, 40));
  });
}

const rows = (log: HTMLElement) => [...log.querySelectorAll("[data-row]")];

describe("LogViewer states", () => {
  it("shows a fixed-height skeleton while the stream opens, then the lines", async () => {
    const h = await mount(
      <LogViewer targets={["mysql/default"]} name="mysql" height={320} />,
      {},
      (fake) => {
        emit(fake, "mysql/default", ["ready for connections"]);
      },
    );
    const loading = screen.getByTestId("logs-loading");
    expect(loading.querySelector("[data-skeleton]")).toHaveStyle({ height: "320px" });
    const log = await screen.findByRole("log", { name: "Log lines of mysql" });
    await waitFor(() => {
      expect(within(log).getByText("ready for connections")).toBeInTheDocument();
    });
    expect(h.fake.calls.length).toBeGreaterThan(0);
  });

  it("says so when there is nothing to show, and when no source is picked", async () => {
    await mount(<LogViewer targets={["mysql/default"]} name="mysql" height={320} />);
    expect(await screen.findByText("No lines to show.")).toBeInTheDocument();
    await harness?.layer.dispose();
    harness = null;
    document.body.innerHTML = "";
    await mount(<LogViewer targets={[]} name="none" height={320} />);
    expect(await screen.findByText("Pick a source to see its lines.")).toBeInTheDocument();
  });

  it("follows live lines, batched, and shows the CLI hint", async () => {
    const h = await mount(<LogViewer targets={["nginx.error"]} name="nginx" height={320} />);
    const log = await screen.findByRole("log");
    emit(h, "nginx.error", ["one", "two", "three"]);
    await flush(h);
    await waitFor(() => {
      expect(rows(log)).toHaveLength(3);
    });
    const hint = document.querySelector("[data-forge-cli]");
    expect(hint).toHaveAttribute("data-forge-cli", "forge logs nginx.error -f");
    await userEvent.setup().type(screen.getByLabelText("Filter"), "upstream");
    expect(document.querySelector("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge logs nginx.error -f --grep upstream",
    );
  });

  it("shows the failure of loading earlier lines", async () => {
    const user = userEvent.setup();
    const h = await mount(
      <LogViewer targets={["mysql/default"]} name="mysql" height={320} />,
      {
        handlers: {
          "logs.read": () => fakeError("internal", "disk unavailable"),
        },
      },
      (fake) => {
        emit(fake, "mysql/default", ["a", "b"]);
      },
    );
    await screen.findByText("a");
    await user.click(screen.getByRole("button", { name: "Load earlier" }));
    await waitFor(() => {
      expect(h.fake.calls.some((c) => c.method === "logs.read")).toBe(true);
    });
    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });
});

describe("pause, lost lines and filters", () => {
  it("buffers new lines while paused and counts them in a badge", async () => {
    const user = userEvent.setup();
    const h = await mount(
      <LogViewer targets={["mysql/default"]} name="mysql" height={320} />,
      {},
      (fake) => {
        emit(fake, "mysql/default", ["first"]);
      },
    );
    const log = await screen.findByRole("log");
    await screen.findByText("first");
    await user.click(screen.getByRole("button", { name: "Pause" }));
    emit(h, "mysql/default", ["while paused 1", "while paused 2"]);
    await flush(h);
    expect(within(log).queryByText("while paused 1")).toBeNull();
    const badge = await screen.findByTestId("logs-new-badge");
    expect(badge).toHaveTextContent("2 new lines");
    await user.click(screen.getByRole("button", { name: "Resume" }));
    expect(await within(log).findByText("while paused 2")).toBeInTheDocument();
    expect(screen.queryByTestId("logs-new-badge")).toBeNull();
  });

  it("marks the lines the stream lost", async () => {
    const h = await mount(
      <LogViewer targets={["mysql/default"]} name="mysql" height={320} />,
      { streamCapacity: 4 },
      (fake) => {
        emit(fake, "mysql/default", ["seed"]);
      },
    );
    const log = await screen.findByRole("log");
    await screen.findByText("seed");
    act(() => {
      h.fake.pauseStreams();
    });
    emit(
      h,
      "mysql/default",
      Array.from({ length: 12 }, (_, i) => `burst ${String(i)}`),
    );
    await act(async () => {
      h.fake.resumeStreams();
      h.fake.flush();
      await new Promise((resolve) => setTimeout(resolve, 40));
    });
    const gap = await waitFor(() => {
      const el = log.querySelector("[data-gap]");
      if (el === null) throw new Error("no marker");
      return el;
    });
    expect(gap.textContent).toMatch(/^\d+ lines? lost$/);
    expect(within(log).getByText("burst 11")).toBeInTheDocument();
  });

  it("filters by text (highlighted) and by level", async () => {
    const user = userEvent.setup();
    const h = await mount(
      <LogViewer targets={["nginx.error"]} name="nginx" height={320} />,
      {},
      (fake) => {
        emit(fake, "nginx.error", ["upstream timed out"], { level: "error" });
        emit(fake, "nginx.error", ["connection closed"], { level: "info" });
      },
    );
    const log = await screen.findByRole("log");
    await screen.findByText("connection closed");
    await user.type(screen.getByLabelText("Filter"), "timed");
    expect(within(log).queryByText("connection closed")).toBeNull();
    expect(log.querySelector("mark")).toHaveTextContent("timed");
    await user.clear(screen.getByLabelText("Filter"));
    expect(await within(log).findByText("connection closed")).toBeInTheDocument();

    await user.click(screen.getByRole("combobox", { name: "Level" }));
    await user.click(await screen.findByRole("option", { name: "Error" }));
    await waitFor(() => {
      expect(within(log).queryByText("connection closed")).toBeNull();
    });
    expect(within(log).getByText("upstream timed out")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(await within(log).findByText("connection closed")).toBeInTheDocument();
    expect(h.count("logs.read")).toBe(0);
  });

  it("loads earlier lines with before_seq and puts them first", async () => {
    const user = userEvent.setup();
    const calls: unknown[] = [];
    const handlers: FakeHandlers = {
      "logs.read": (params) => {
        calls.push(params);
        const lines: T.LogLineDto[] = [1, 2, 3, 4, 5].map((seq) => ({
          seq,
          ts: null,
          source: "stdout",
          level: null,
          text: `ancient ${String(seq)}`,
        }));
        return { target: params.target, lines, last_seq: 205 };
      },
    };
    await mount(
      <LogViewer targets={["mysql/default"]} name="mysql" height={320} />,
      { handlers },
      (fake) => {
        // 205 lines: the tail of 200 starts at seq 6, so seq 1..5 are "earlier".
        emit(
          fake,
          "mysql/default",
          Array.from({ length: 205 }, (_, i) => `recent ${String(i + 1)}`),
        );
      },
    );
    const log = await screen.findByRole("log");
    // jsdom has no layout: the list stays at its top, where the tail starts.
    await screen.findByText("recent 6");
    await user.click(screen.getByRole("button", { name: "Load earlier" }));
    await waitFor(() => {
      expect(calls).toEqual([{ target: "mysql/default", limit: 200, before_seq: 6 }]);
    });
    // Fewer than a page came back: nothing older exists.
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Load earlier" })).toBeDisabled();
    });
    const first = await waitFor(() => {
      const el = log.querySelector("[data-row]");
      if (el?.textContent.includes("ancient 1") !== true) throw new Error("not first yet");
      return el;
    });
    expect(first).toHaveTextContent("ancient 1");
  });
});

describe("access lines and file links", () => {
  const ACCESS: T.AccessLineDto = {
    host: "loja.test",
    status: 502,
    method: "GET",
    uri: "/checkout",
    rt_ms: 12.5,
    upstream: "127.0.0.1:19083",
  };

  it("shows the access fields as columns and offers the site filter", async () => {
    const user = userEvent.setup();
    await mount(<LogViewer targets={["site:loja.test"]} name="loja" height={320} />, {}, (fake) => {
      emit(fake, "site:loja.test", ["loja.test GET /checkout 502"], {
        ts: "2026-01-01T10:00:05Z",
        access: ACCESS,
        level: "error",
      });
    });
    const log = await screen.findByRole("log");
    const row = await waitFor(() => {
      const el = log.querySelector("[data-row]");
      if (el === null) throw new Error("no row");
      return el;
    });
    const col = (name: string) => row.querySelector(`[data-col="${name}"]`)?.textContent;
    expect(col("time")).toBe("10:00:05");
    expect(col("status")).toBe("502");
    expect(col("method")).toBe("GET");
    expect(col("uri")).toBe("/checkout");
    expect(col("host")).toBe("loja.test");
    expect(col("ms")).toBe("12.5 ms");
    expect(col("upstream")).toBe("127.0.0.1:19083");
    expect(screen.getByRole("combobox", { name: "Site" })).toBeInTheDocument();
    await user.click(screen.getByRole("combobox", { name: "Site" }));
    expect(await screen.findByRole("option", { name: "loja.test" })).toBeInTheDocument();
  });

  const TRACE = "#3 C:\\Forgebox\\www\\loja\\app\\Cart.php(87): Cart->total()";

  it("opens a stack-trace frame in the editor", async () => {
    const user = userEvent.setup();
    const h = await mount(
      <LogViewer targets={["php@8.4"]} name="php" height={320} />,
      { handlers: { "shell.open_editor": () => ({ ok: true, message: null }) } },
      (fake) => {
        emit(fake, "php@8.4", [TRACE]);
      },
    );
    const link = await screen.findByRole("button", {
      name: "C:\\Forgebox\\www\\loja\\app\\Cart.php(87)",
    });
    await user.click(link);
    await waitFor(() => {
      expect(h.fake.calls.find((c) => c.method === "shell.open_editor")?.params).toEqual({
        path: "C:\\Forgebox\\www\\loja\\app\\Cart.php",
        line: 87,
      });
    });
    // "Open file" opens the first frame of the selected lines.
    const open = screen.getByRole("button", { name: "Open file" });
    expect(open).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Select line" }));
    expect(open).toBeEnabled();
  });

  it("does not link files when the daemon is remote, and explains why", async () => {
    await mount(
      <LogViewer targets={["php@8.4"]} name="php" height={320} />,
      { hello: { local: false, role: "admin" } },
      (fake) => {
        emit(fake, "php@8.4", [TRACE]);
      },
    );
    const log = await screen.findByRole("log");
    const hidden = await waitFor(() => {
      const el = log.querySelector("[data-remote-link]");
      if (el === null) throw new Error("no text");
      return el;
    });
    expect(hidden).toHaveAttribute(
      "title",
      "Opening files in the editor only works on the machine that runs the daemon.",
    );
    expect(log.querySelector("[data-file-link]")).toBeNull();
    expect(screen.queryByRole("button", { name: "Open file" })).toBeNull();
    expect(screen.getByTestId("logs-remote-note")).toBeInTheDocument();
  });
});

describe("TargetPicker", () => {
  const targets: T.LogTargetDto[] = Array.from({ length: 10 }, (_, i) => ({
    target: `nginx.log${String(i)}`,
    label: `nginx log ${String(i)}`,
    kind: "instance_file",
    group: i < 6 ? "nginx" : "mysql",
  }));

  it("groups the sources and stops at eight", async () => {
    const user = userEvent.setup();
    let selected: string[] = [];
    const ui = (value: string[]) => (
      <TargetPicker
        selected={value}
        onChange={(next) => {
          selected = next;
        }}
      />
    );
    await mount(ui([]), { handlers: { "logs.targets": () => ({ targets }) } });
    const group = await screen.findByRole("group", { name: "Sources" });
    expect(
      [...group.querySelectorAll("fieldset")].map((f) => f.getAttribute("data-group")),
    ).toEqual(["nginx", "mysql"]);
    await user.click(screen.getByRole("checkbox", { name: "nginx log 0" }));
    expect(selected).toEqual(["nginx.log0"]);
  });

  it("disables the rest once eight are selected", async () => {
    const chosen = targets.slice(0, 8).map((t) => t.target);
    await mount(<TargetPicker selected={chosen} onChange={() => undefined} />, {
      handlers: { "logs.targets": () => ({ targets }) },
    });
    expect(await screen.findByRole("checkbox", { name: "nginx log 9" })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: "nginx log 0" })).toBeEnabled();
    expect(screen.getByText("At most 8 sources at once.")).toBeInTheDocument();
  });

  it("shows the empty and the error state", async () => {
    await mount(<TargetPicker selected={[]} onChange={() => undefined} />, {
      handlers: { "logs.targets": () => ({ targets: [] }) },
    });
    expect(await screen.findByText(/No log sources yet/)).toBeInTheDocument();
    await harness?.layer.dispose();
    harness = null;
    document.body.innerHTML = "";
    await mount(<TargetPicker selected={[]} onChange={() => undefined} />, {
      handlers: { "logs.targets": () => fakeError("internal", "boom") },
    });
    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });
});
