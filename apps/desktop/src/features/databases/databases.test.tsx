// Acceptance of M2-E3-T14 (docs/07 section 5.6): the instance states, create and drop with the
// daemon's rules, the masked connection strings, export and import jobs (progress, cancel,
// failure), the shell in the integrated terminal, the remote limits and the site "Banco" tab.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createDatabasesQa,
  qaDatabase,
  type DatabasesQa,
} from "../../../qa/fixtures/scenarios/databases";
import { siteTabs } from "../../app/slots";
import { renderApp, type AppHarness } from "../../app/testApp";
import { populatedData, site as siteDto, type ScenarioData } from "../../state/testFixtures";
import { resetTerminalsForTest } from "../../state/terminals";
import { resetTransfersForTest } from "./transfers";

const native = vi.hoisted(() => ({
  copyText: vi.fn(() => Promise.resolve(true)),
  pasteText: vi.fn(() => Promise.resolve(null)),
  pickPath: vi.fn(() => Promise.resolve(null)),
  openUrl: vi.fn(() => Promise.resolve(true)),
  revealPath: vi.fn(() => Promise.resolve(true)),
  onPathsDropped: vi.fn(() => Promise.resolve(() => undefined)),
}));
vi.mock("../../ipc/native", () => native);

const terminal = vi.hoisted(() => ({ opened: [] as unknown[][] }));
vi.mock("../../state/terminals", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../state/terminals")>();
  return {
    ...original,
    openTerminal: vi.fn((...args: unknown[]) => {
      terminal.opened.push(args);
      return (original.openTerminal as (...a: unknown[]) => string)(...args);
    }),
  };
});

/** The value or a failing test (no non-null assertions). */
function must<T>(value: T | null | undefined): T {
  if (value === null || value === undefined) throw new Error("expected a value");
  return value;
}

const SITE_ID = "01JSITE00000000000000000SHP";
let harness: AppHarness | null = null;

beforeEach(() => {
  terminal.opened.length = 0;
  Object.values(native).forEach((spy) => spy.mockClear());
});

afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
  resetTransfersForTest();
  resetTerminalsForTest();
});

async function open(
  qa: DatabasesQa = createDatabasesQa(),
  options: { path?: string; scenario?: string; data?: ScenarioData } = {},
): Promise<{ h: AppHarness; qa: DatabasesQa }> {
  harness = await renderApp({
    path: options.path ?? "/databases",
    ...(options.scenario === undefined ? {} : { scenario: options.scenario }),
    ...(options.data === undefined ? {} : { data: options.data }),
    extra: { notImplemented: [], latencyMs: 0, handlers: qa.handlers },
  });
  return { h: harness, qa };
}

const calls = (method: string) =>
  harness?.fake.calls.filter((call) => call.method === method) ?? [];

function row(name: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-database="${name}"]`);
  if (el === null) throw new Error(`no row ${name}`);
  return el;
}

function card(name: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-instance="${name}"]`);
  if (el === null) throw new Error(`no card ${name}`);
  return el;
}

function cli(text: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-forge-cli="${text}"]`);
}

describe("instances", () => {
  it("shows the no-instance state with a way to the catalog", async () => {
    await open(createDatabasesQa({ empty: true }));
    expect(await screen.findByText("No database installed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open the catalog" })).toBeInTheDocument();
  });

  it("R-16: a daemon without db.list explains it instead of an endless skeleton", async () => {
    harness = await renderApp({
      path: "/databases",
      extra: { notImplemented: ["db.list"], latencyMs: 0, handlers: createDatabasesQa().handlers },
    });
    expect(await screen.findByText("Not available yet in this version of Forgebox.")).toBeVisible();
    expect(document.querySelector("[data-skeleton]")).toBeNull();
    expect(screen.queryByRole("button", { name: "Open the catalog" })).toBeNull();
  });

  it("R-26: the row actions are one non-wrapping line of quiet buttons", async () => {
    await open();
    const first = document.querySelector<HTMLElement>("[data-database]");
    const actions = first?.querySelector("td:last-child > div");
    expect(actions?.className).toContain("flex-nowrap");
    const variants = [...(actions?.querySelectorAll("button") ?? [])].map((b) =>
      b.getAttribute("data-variant"),
    );
    expect(variants.length).toBeGreaterThan(2);
    expect(new Set(variants)).toEqual(new Set(["ghost", "destructive"]));
  });

  it("shows a stopped instance with a start call to action that calls services.start", async () => {
    const user = userEvent.setup();
    await open(createDatabasesQa({ running: false }));
    expect(await screen.findByText("mysql/default is stopped")).toBeInTheDocument();
    const mysql = card("mysql/default");
    expect(within(mysql).getByText("Stopped")).toBeInTheDocument();
    expect(screen.queryByTestId("create-database")).toBeNull();
    await user.click(within(mysql).getByRole("button", { name: "Start" }));
    await waitFor(() => {
      expect(calls("services.start")[0]?.params).toEqual({ targets: ["mysql/default"] });
    });
    // The list follows the instance: the databases appear once it runs.
    await waitFor(() => {
      expect(screen.getByTestId("database-table")).toBeInTheDocument();
    });
    expect(within(card("mysql/default")).queryByRole("button", { name: "Start" })).toBeNull();
  });

  it("lists the instances with version, state, port and size (≈ while computing)", async () => {
    await open();
    await screen.findByTestId("instance-cards");
    const mysql = card("mysql/default");
    expect(within(mysql).getByText("Version 8.4.13")).toBeInTheDocument();
    expect(within(mysql).getByText("Running")).toBeInTheDocument();
    expect(within(mysql).getByText("Port 3306")).toBeInTheDocument();
    expect(within(mysql).getByText(/^Data 412/)).toBeInTheDocument();
    const postgres = card("postgresql/default");
    expect(within(postgres).getByText("≈ calculating…")).toBeInTheDocument();
    expect(within(postgres).getByText("Stopped")).toBeInTheDocument();
    expect(within(postgres).getByRole("button", { name: "Start" })).toBeInTheDocument();
  });

  it("lists the databases with charset, size and tables, names case-sensitive", async () => {
    await open();
    await screen.findByTestId("database-table");
    const names = [...document.querySelectorAll<HTMLElement>("[data-database]")].map(
      (el) => el.dataset.database,
    );
    expect(names).toEqual(["Loja", "blog", "loja"]);
    const loja = within(row("loja"));
    expect(loja.getByText("utf8mb4 / utf8mb4_0900_ai_ci")).toBeInTheDocument();
    expect(loja.getByText("42")).toBeInTheDocument();
    expect(loja.getByText(/^120/)).toBeInTheDocument();
  });

  it("shows the instance's databases when another card is chosen", async () => {
    const user = userEvent.setup();
    await open();
    await screen.findByTestId("database-table");
    await user.click(screen.getByRole("radio", { name: "Show databases of postgresql/default" }));
    expect(await screen.findByText("postgresql/default is stopped")).toBeInTheDocument();
  });

  it("keeps 'Abrir em' disabled as 'soon' and hides Redis, users and backups", async () => {
    await open();
    await screen.findByTestId("database-table");
    const openIn = screen.getByRole("button", { name: /Open in/ });
    expect(openIn).toBeDisabled();
    expect(openIn).toHaveTextContent("soon");
    expect(screen.queryByText(/redis/i)).toBeNull();
    expect(screen.queryByText(/users/i)).toBeNull();
    expect(screen.queryByText(/backup/i)).toBeNull();
  });
});

describe("create", () => {
  it("shows an inline error for an invalid name before any call", async () => {
    const user = userEvent.setup();
    await open();
    const form = await screen.findByTestId("create-database");
    await user.type(within(form).getByLabelText("Name"), "my db!");
    expect(await within(form).findByRole("alert")).toHaveTextContent(
      "Use only letters, digits and underscore",
    );
    expect(within(form).getByRole("button", { name: "Create" })).toBeDisabled();
    await user.click(within(form).getByRole("button", { name: "Create" }));
    expect(calls("db.create")).toHaveLength(0);
    expect(within(form).queryByText(/^forge db create/)).toBeNull();
  });

  it("rejects a name longer than 64 characters and one that already exists", async () => {
    const user = userEvent.setup();
    await open();
    const form = await screen.findByTestId("create-database");
    const input = within(form).getByLabelText("Name");
    await user.type(input, "a".repeat(65));
    expect(await within(form).findByRole("alert")).toHaveTextContent("up to 64");
    await user.clear(input);
    await user.type(input, "blog");
    expect(await within(form).findByRole("alert")).toHaveTextContent("already exists");
    await user.clear(input);
    await user.type(input, "BLOG");
    await waitFor(() => {
      expect(within(form).queryByRole("alert")).toBeNull();
    });
    expect(calls("db.create")).toHaveLength(0);
  });

  it("creates the database with the chosen charset and shows the CLI command", async () => {
    const user = userEvent.setup();
    const { qa } = await open();
    const form = await screen.findByTestId("create-database");
    await user.type(within(form).getByLabelText("Name"), "shop");
    expect(cli("forge db create shop --on mysql/default --charset utf8mb4")).not.toBeNull();
    await user.click(within(form).getByRole("button", { name: "Create" }));
    await waitFor(() => {
      expect(calls("db.create")[0]?.params).toEqual({
        name: "shop",
        instance: "mysql/default",
        charset: "utf8mb4",
      });
    });
    expect(qa.databases.some((db) => db.name === "shop")).toBe(true);
    await waitFor(() => {
      expect(document.querySelector('[data-database="shop"]')).not.toBeNull();
    });
  });
});

describe("drop", () => {
  it("needs the exact typed name and then sends confirm", async () => {
    const user = userEvent.setup();
    const { qa } = await open();
    await screen.findByTestId("database-table");
    await user.click(within(row("loja")).getByRole("button", { name: "Drop loja" }));
    const dialog = await screen.findByTestId("drop-database-dialog");
    const confirm = within(dialog).getByRole("button", { name: "Drop database" });
    expect(confirm).toBeDisabled();
    expect(within(dialog).getByText("forge db drop loja --on mysql/default")).toBeInTheDocument();
    const field = within(dialog).getByLabelText("Type loja to confirm");
    await user.type(field, "Loja");
    expect(confirm).toBeDisabled();
    await user.clear(field);
    await user.type(field, "loja");
    expect(confirm).toBeEnabled();
    expect(calls("db.drop")).toHaveLength(0);
    await user.click(confirm);
    await waitFor(() => {
      expect(calls("db.drop")[0]?.params).toEqual({
        name: "loja",
        instance: "mysql/default",
        confirm: true,
      });
    });
    expect(qa.databases.map((db) => db.name)).toEqual(["blog", "Loja"]);
  });
});

describe("connection strings", () => {
  it("masks the credentials until revealed and shows DSN, URL and .env", async () => {
    const user = userEvent.setup();
    await open();
    await screen.findByTestId("database-table");
    await user.click(within(row("loja")).getByRole("button", { name: "Connection" }));
    const url = await screen.findByTestId("connection-url");
    expect(url).toHaveTextContent("mysql://root:••••••••@127.0.0.1:3306/loja");
    expect(url).not.toHaveTextContent("s3cr3t");
    expect(screen.getByTestId("connection-env")).not.toHaveTextContent("s3cr3t");
    expect(screen.getByTestId("connection-env")).toHaveTextContent("DB_DATABASE=loja");
    expect(screen.getByTestId("connection-dsn")).toHaveTextContent(
      "mysql:host=127.0.0.1;port=3306;dbname=loja",
    );
    expect(cli("forge db url mysql/default loja")).not.toBeNull();
    // Copy waits for the reveal.
    const copyUrl = within(
      must(document.querySelector<HTMLElement>('[data-connection="url"]')),
    ).getByRole("button", { name: "Copy" });
    expect(copyUrl).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "Reveal" }));
    expect(screen.getByTestId("connection-url")).toHaveTextContent(
      "mysql://root:s3cr3t@127.0.0.1:3306/loja",
    );
    expect(screen.getByTestId("connection-env")).toHaveTextContent("DB_PASSWORD=s3cr3t");
    await user.click(copyUrl);
    expect(native.copyText).toHaveBeenCalledWith("mysql://root:s3cr3t@127.0.0.1:3306/loja");
    await user.click(screen.getByRole("button", { name: "Hide" }));
    expect(screen.getByTestId("connection-url")).not.toHaveTextContent("s3cr3t");
  });

  it("does not keep the strings in the query cache or the stores", async () => {
    const user = userEvent.setup();
    const { h } = await open();
    await screen.findByTestId("database-table");
    await user.click(within(row("loja")).getByRole("button", { name: "Connection" }));
    await screen.findByTestId("connection-url");
    const runtime = h.layer.runtime("local");
    expect(JSON.stringify(runtime.stores.jobs.getState())).not.toContain("s3cr3t");
    expect(JSON.stringify(runtime.stores.services.getState())).not.toContain("s3cr3t");
  });
});

describe("shell", () => {
  it("calls db.shell and then openTerminal with the argv", async () => {
    const user = userEvent.setup();
    await open();
    await screen.findByTestId("database-table");
    const button = within(row("loja")).getByRole("button", { name: "Open shell" });
    expect(button.closest("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge db shell mysql/default loja",
    );
    await user.click(button);
    await waitFor(() => {
      expect(calls("db.shell")[0]?.params).toEqual({ instance: "mysql/default", database: "loja" });
    });
    await waitFor(() => {
      expect(terminal.opened).toHaveLength(1);
    });
    const [options, context] = terminal.opened[0] ?? [];
    expect(options).toMatchObject({
      command: [
        "C:/Forgebox/bin/mysql/8.4.13/bin/mysql.exe",
        "--protocol=tcp",
        "--host=127.0.0.1",
        "--port=3306",
        "--user=root",
        "loja",
      ],
      title: "loja shell",
      env: { MYSQL_PWD: "s3cr3t" },
    });
    expect(context).toBe("local");
  });

  it("is disabled with a reason on a remote daemon (db.shell is local only)", async () => {
    await open(createDatabasesQa(), { scenario: "remote" });
    await screen.findByTestId("database-table");
    const wrapper = within(row("loja"))
      .getByRole("button", { name: "Open shell" })
      .closest("[data-action]");
    expect(wrapper).toHaveAttribute("data-disabled-reason");
  });
});

describe("export", () => {
  it("shows bytes and speed, cancels, and ends 'cancelled'", async () => {
    const user = userEvent.setup();
    const qa = createDatabasesQa();
    const mysql = must(qa.instances[0]);
    qa.databases.push(qaDatabase(mysql, "hold_big", 5, 1));
    await open(qa);
    await screen.findByTestId("database-table");
    expect(cli("forge db export hold_big --on mysql/default")).not.toBeNull();
    await user.click(within(row("hold_big")).getByRole("button", { name: "Export" }));
    const transfer = await screen.findByTestId("transfers");
    expect(await within(transfer).findByText("3 MB written · 6 MB/s")).toBeInTheDocument();
    await user.click(within(transfer).getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(calls("jobs.cancel")).toHaveLength(1);
    });
    expect(await within(transfer).findByText("Export cancelled.")).toBeInTheDocument();
  });

  it("finishes with the path, size and 'Show in folder' when local", async () => {
    const user = userEvent.setup();
    await open();
    await screen.findByTestId("database-table");
    await user.click(within(row("blog")).getByRole("button", { name: "Export" }));
    const done = await screen.findByText(/^Exported to C:\/Forgebox\/backups/);
    expect(done).toHaveTextContent("15 MB");
    await user.click(screen.getByRole("button", { name: "Show in folder" }));
    expect(native.revealPath).toHaveBeenCalledWith(
      "C:/Forgebox/backups/mysql/default/blog-20261104T120000Z.sql.zst",
    );
  });

  it("shows the stderr tail and retries when it fails", async () => {
    const user = userEvent.setup();
    const qa = createDatabasesQa();
    qa.databases.push(qaDatabase(must(qa.instances[0]), "broken_db", 1, 1));
    await open(qa);
    await screen.findByTestId("database-table");
    await user.click(within(row("broken_db")).getByRole("button", { name: "Export" }));
    const failure = await screen.findByTestId("transfer-failure");
    expect(within(failure).getByText("The export failed")).toBeInTheDocument();
    expect(within(failure).getByTestId("transfer-stderr")).toHaveTextContent("Access denied");
    expect(within(failure).getByTestId("transfer-stderr")).not.toHaveTextContent("client failed");
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => {
      expect(calls("db.export")).toHaveLength(2);
    });
    await screen.findByTestId("transfer-failure");
    expect(document.querySelectorAll("[data-transfer]")).toHaveLength(1);
  });

  it("hides 'Show in folder' on a remote daemon", async () => {
    const user = userEvent.setup();
    await open(createDatabasesQa(), { scenario: "remote" });
    await screen.findByTestId("database-table");
    await user.click(within(row("blog")).getByRole("button", { name: "Export" }));
    await screen.findByText(/^Exported to /);
    expect(screen.queryByRole("button", { name: "Show in folder" })).toBeNull();
  });
});

describe("import", () => {
  it("validates the file, shows the percentage and cancels", async () => {
    const user = userEvent.setup();
    const qa = createDatabasesQa();
    qa.databases.push(qaDatabase(must(qa.instances[0]), "hold_in", 0, 0));
    await open(qa);
    await screen.findByTestId("database-table");
    await user.click(within(row("hold_in")).getByRole("button", { name: "Import" }));
    const sheet = await screen.findByTestId("import-sheet");
    expect(within(sheet).getByRole("button", { name: "Browse…" })).toBeInTheDocument();
    expect(within(sheet).queryByTestId("import-remote-note")).toBeNull();
    const field = within(sheet).getByLabelText("Dump file");
    await user.type(field, "C:\\dumps\\x.txt");
    expect(await within(sheet).findByText("Use a .sql, .sql.gz or .sql.zst file.")).toBeVisible();
    await user.click(within(sheet).getByRole("button", { name: "Import" }));
    expect(calls("db.import")).toHaveLength(0);
    await user.clear(field);
    await user.type(field, "C:\\dumps\\x.sql.zst");
    expect(
      cli("forge db import hold_in C:\\dumps\\x.sql.zst --on mysql/default") ??
        screen.getByText(/^forge db import hold_in/),
    ).toBeTruthy();
    await user.click(within(sheet).getByRole("button", { name: "Import" }));
    await waitFor(() => {
      expect(calls("db.import")[0]?.params).toEqual({
        name: "hold_in",
        instance: "mysql/default",
        file: "C:\\dumps\\x.sql.zst",
      });
    });
    await waitFor(() => {
      expect(screen.queryByTestId("import-sheet")).toBeNull();
    });
    const transfer = await screen.findByTestId("transfers");
    expect(await within(transfer).findByText("35%")).toBeInTheDocument();
    await user.click(within(transfer).getByRole("button", { name: "Cancel" }));
    expect(await within(transfer).findByText("Import cancelled.")).toBeInTheDocument();
  });

  it("is limited to www/ on a remote daemon, with the explanation", async () => {
    const user = userEvent.setup();
    await open(createDatabasesQa(), { scenario: "remote" });
    await screen.findByTestId("database-table");
    await user.click(within(row("blog")).getByRole("button", { name: "Import" }));
    const sheet = await screen.findByTestId("import-sheet");
    expect(within(sheet).getByTestId("import-remote-note")).toHaveTextContent("www/");
    expect(within(sheet).queryByRole("button", { name: "Browse…" })).toBeNull();
    const field = within(sheet).getByLabelText("Dump file");
    await user.type(field, "D:/dumps/x.sql");
    expect(
      await within(sheet).findByText("On a remote daemon the file must be inside www/."),
    ).toBeVisible();
    await user.click(within(sheet).getByRole("button", { name: "Import" }));
    expect(calls("db.import")).toHaveLength(0);
    await user.clear(field);
    await user.type(field, "C:/Forgebox/www/dumps/x.sql");
    await user.click(within(sheet).getByRole("button", { name: "Import" }));
    await waitFor(() => {
      expect(calls("db.import")).toHaveLength(1);
    });
  });

  it("reports a failed import with the client output", async () => {
    const user = userEvent.setup();
    const qa = createDatabasesQa();
    qa.databases.push(qaDatabase(must(qa.instances[0]), "broken_in", 0, 0));
    await open(qa);
    await screen.findByTestId("database-table");
    await user.click(within(row("broken_in")).getByRole("button", { name: "Import" }));
    const sheet = await screen.findByTestId("import-sheet");
    await user.type(within(sheet).getByLabelText("Dump file"), "C:\\d\\x.sql");
    await user.click(within(sheet).getByRole("button", { name: "Import" }));
    const failure = await screen.findByTestId("transfer-failure");
    expect(within(failure).getByTestId("transfer-stderr")).toHaveTextContent("ERROR 1064");
  });
});

describe("site 'Banco' tab", () => {
  function siteData(): ScenarioData {
    const data = populatedData();
    data.sites = [siteDto(SITE_ID, "shop.test", {})];
    return data;
  }

  it("is registered in the slot registry", async () => {
    await open(createDatabasesQa(), { path: `/sites/${SITE_ID}`, data: siteData() });
    await screen.findByRole("heading", { level: 1 });
    const tab = siteTabs.list().find((entry) => entry.id === "database");
    expect(tab).toBeDefined();
    expect(tab?.label).toEqual({ ns: "databases", key: "siteTab.label" });
    expect(screen.getByRole("tab", { name: "Database" })).toBeInTheDocument();
  });

  it("creates the database of the site and then shows its connection strings", async () => {
    const user = userEvent.setup();
    await open(createDatabasesQa(), { path: `/sites/${SITE_ID}`, data: siteData() });
    await user.click(await screen.findByRole("tab", { name: "Database" }));
    const tab = await screen.findByTestId("site-database-tab");
    const form = await within(tab).findByTestId("create-database");
    expect(within(form).getByLabelText("Name")).toHaveValue("shop");
    await user.click(within(form).getByRole("button", { name: "Create" }));
    await waitFor(() => {
      expect(calls("db.create")[0]?.params).toMatchObject({
        name: "shop",
        instance: "mysql/default",
      });
    });
    expect(await within(tab).findByText("Database shop exists.")).toBeInTheDocument();
    expect(await within(tab).findByTestId("connection-url")).toHaveTextContent("••••••••");
    expect(within(tab).getByTestId("connection-url")).not.toHaveTextContent("s3cr3t");
  });

  it("shows the connection strings right away when the database exists", async () => {
    const data = siteData();
    data.sites = [siteDto(SITE_ID, "loja.test", {})];
    await open(createDatabasesQa(), { path: `/sites/${SITE_ID}`, data });
    await userEvent.setup().click(await screen.findByRole("tab", { name: "Database" }));
    const tab = await screen.findByTestId("site-database-tab");
    expect(await within(tab).findByText("Database loja exists.")).toBeInTheDocument();
    expect(await within(tab).findByTestId("connection-dsn")).toBeInTheDocument();
  });

  it("offers to start a stopped instance", async () => {
    await open(createDatabasesQa({ running: false }), {
      path: `/sites/${SITE_ID}`,
      data: siteData(),
    });
    await userEvent.setup().click(await screen.findByRole("tab", { name: "Database" }));
    const tab = await screen.findByTestId("site-database-tab");
    expect(
      await within(tab).findByText("mysql/default is stopped. Start it to create the database."),
    ).toBeInTheDocument();
    await act(async () => {
      await userEvent.setup().click(within(tab).getByRole("button", { name: "Start" }));
    });
    await waitFor(() => {
      expect(calls("services.start")).toHaveLength(1);
    });
  });
});
