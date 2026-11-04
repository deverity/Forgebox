// Acceptance of M2-E3-T20: masked by default, reveal through sites.env {reveal:true} that
// hides again after 30 s, role gating, values only in component state, empty/error states.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configHandlers } from "../../../../../qa/fixtures/scenarios/config";
import { QA_ENV_SECRET, siteDetailHandlers } from "../../../../../qa/fixtures/scenarios/siteDetail";
import { renderApp, type AppHarness } from "../../../../app/testApp";
import { fakeError, type FakeHandlers } from "../../../../ipc/fakeBridge";
import type * as T from "../../../../ipc/generated";
import { populatedData, site as siteDto } from "../../../../state/testFixtures";
import { REVEAL_MS } from "./EnvTab";

const SITE_ID = "01JSITE00000000000000000LJA";
const OTHER_ID = "01JSITE00000000000000000OTH";
let harness: AppHarness | null = null;

afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  harness = null;
  vi.restoreAllMocks();
});

async function open(
  options: { handlers?: FakeHandlers; hello?: Partial<T.HelloDto>; site?: string } = {},
): Promise<AppHarness> {
  const data = populatedData();
  data.sites = [siteDto(SITE_ID, "loja.test", {}), siteDto(OTHER_ID, "vazio.test", {})];
  harness = await renderApp({
    path: `/sites/${options.site ?? SITE_ID}`,
    data,
    extra: {
      notImplemented: [],
      ...(options.hello === undefined ? {} : { hello: options.hello }),
      handlers: { ...configHandlers(data), ...siteDetailHandlers(data), ...options.handlers },
    },
  });
  await screen.findByRole("heading", { level: 1 });
  await userEvent.setup().click(await screen.findByRole("tab", { name: "Environment" }));
  return harness;
}

const calls = (method: string) =>
  harness?.fake.calls.filter((call) => call.method === method) ?? [];

describe("Ambiente", () => {
  it("masks secrets by default and never asks for them", async () => {
    await open();
    const tab = await screen.findByTestId("site-env-tab");
    expect(within(tab).getByText("local")).toBeInTheDocument();
    expect(within(tab).getAllByTestId("env-masked")).toHaveLength(2);
    expect(within(tab).queryByText(QA_ENV_SECRET)).toBeNull();
    expect(calls("sites.env").every((c) => (c.params as T.SiteEnvParams).reveal !== true)).toBe(
      true,
    );
    expect(within(tab).getByText("forge site env loja.test")).toBeInTheDocument();
  });

  it("reveals one value with an explicit action, copies it, and re-masks after 30 s", async () => {
    await open();
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"], shouldAdvanceTime: true });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });

    // Copy is offered only for values in the clear.
    expect(screen.queryByRole("button", { name: "Copy value of DB_PASSWORD" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Reveal DB_PASSWORD" }));
    expect(await screen.findByText(QA_ENV_SECRET)).toBeInTheDocument();
    expect(calls("sites.env").some((c) => (c.params as T.SiteEnvParams).reveal === true)).toBe(
      true,
    );
    // Only the asked value is shown; the other secret stays masked.
    expect(screen.queryByText("base64:qa-app-key")).toBeNull();
    expect(screen.getAllByTestId("env-masked")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Copy value of DB_PASSWORD" }));
    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith(QA_ENV_SECRET);
    });

    act(() => {
      vi.advanceTimersByTime(REVEAL_MS - 1000);
    });
    expect(screen.getByText(QA_ENV_SECRET)).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(1500);
    });
    expect(screen.queryByText(QA_ENV_SECRET)).toBeNull();
    expect(screen.getAllByTestId("env-masked")).toHaveLength(2);
  });

  it("never lets a revealed value reach the stores or the query cache", async () => {
    const h = await open();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Reveal DB_PASSWORD" }));
    await screen.findByText(QA_ENV_SECRET);
    const runtime = h.layer.runtime("local");
    for (const store of Object.values(runtime.stores) as { getState(): unknown }[]) {
      expect(JSON.stringify(store.getState())).not.toContain(QA_ENV_SECRET);
    }
    const cache = h.layer.queryClient
      .getQueryCache()
      .getAll()
      .map((q) => JSON.stringify({ key: q.queryKey, data: q.state.data }))
      .join("\n");
    expect(cache).not.toContain(QA_ENV_SECRET);
    expect(cache).toContain("DB_PASSWORD");
  });

  it("hides reveal with a reason for a non-admin role", async () => {
    await open({ hello: { role: "operator" } });
    const button = await screen.findByRole("button", { name: "Reveal DB_PASSWORD" });
    expect(button).toBeDisabled();
    expect(screen.getByTestId("env-reveal-reason")).toHaveTextContent(
      "requires the admin role (this connection is operator)",
    );
    expect(calls("sites.env").some((c) => (c.params as T.SiteEnvParams).reveal === true)).toBe(
      false,
    );
  });

  it("opens the file in the editor", async () => {
    await open();
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Open .env in editor" }));
    await waitFor(() => {
      expect(calls("shell.open_editor")[0]?.params).toEqual({
        path: "C:/Forgebox/www/loja.test/.env",
      });
    });
  });

  it("disables the editor action when the connection is not local", async () => {
    await open({ hello: { local: false } });
    expect(await screen.findByRole("button", { name: "Open .env in editor" })).toBeDisabled();
  });

  it("shows the empty state when the site has no .env", async () => {
    await open({ site: OTHER_ID });
    expect(await screen.findByText("No .env file")).toBeInTheDocument();
    expect(screen.getByText("forge site env vazio.test")).toBeInTheDocument();
  });

  it("shows an error when the file is unreadable, and retries", async () => {
    let failing = true;
    await open({
      handlers: {
        "sites.env": (params) => {
          if (failing) fakeError("io", "cannot read C:/Forgebox/www/loja.test/.env");
          return {
            site_id: SITE_ID,
            host: "loja.test",
            files: [],
            revealed: params.reveal === true,
          };
        },
      },
    });
    const retry = await screen.findByRole("button", { name: "Try again" }, { timeout: 4000 });
    failing = false;
    await userEvent.setup().click(retry);
    expect(await screen.findByText("No .env file")).toBeInTheDocument();
  });
});
