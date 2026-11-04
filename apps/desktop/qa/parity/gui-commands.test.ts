// GUI/CLI parity gate, TypeScript half (M2-E4-T5, docs/07 §1.7, ADR-0037).
// Every action of the registry is run with sample inputs; the `forge` command it renders is
// written to `gui-commands.json` (committed). The Rust half, `crates/forge-cli/tests/gui_parity.rs`,
// parses every command with the real clap tree and checks the coverage of the METHODS table.
// Regenerate with `FORGE_PARITY_UPDATE=1 pnpm -C apps/desktop exec vitest run qa/parity`;
// without it the test fails when the committed file differs (drift check, also run by CI).
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { listActions } from "../../src/actions/registry";
import { CliCommand } from "../../src/design/primitives/CliCommand";
import { renderCli } from "../../src/ipc/cli";

// Loading every module of src/actions registers its actions. A glob, not a list of imports: a
// screen that adds `src/actions/<name>.ts` is covered here without editing this file (a missed
// import once left the db.* actions out of gui-commands.json while their screen shipped).
// Untyped call on purpose (Vitest fsModuleCache, see src/test/globCache.test.ts).
const actionModules: Record<string, unknown> = import.meta.glob(
  ["../../src/actions/*.ts", "!../../src/actions/*.test.ts"],
  { eager: true },
);

const here = dirname(fileURLToPath(import.meta.url));
const JSON_PATH = join(here, "gui-commands.json");

const SPACED = String.raw`C:\Meus Projetos\loja virtual`;

/** Sample screen inputs per action id (the input type of `defineAction`). */
const SAMPLES: Record<string, unknown[]> = {
  "services.start": [undefined],
  "services.stop": [undefined],
  "services.start_instance": ["mysql/default"],
  "services.stop_instance": ["mysql/default"],
  "services.restart_instance": ["nginx"],
  "services.set_port": [{ target: "mysql/default", port: 3307 }],
  "services.create_instance": [
    { tool: "mysql@8.4.3", name: "legacy", port: 3307, autostart: true, dataDir: "" },
    { tool: "redis", name: "cache", port: null, autostart: false, dataDir: "" },
    {
      tool: "mysql@8.4.3",
      name: "legacy",
      port: null,
      autostart: false,
      dataDir: String.raw`D:\Dados MySQL\legacy`,
    },
  ],
  "services.remove_instance": [{ instance: "mysql/legacy", deleteData: true }],
  "services.kill_port_holder": [{ port: 80, pid: 4412 }],
  "services.set_settings": [
    { target: "php@8.4", settings: { memory_limit: "1G", max_execution_time: "60" } },
  ],
  "services.open_config": ["nginx"],
  "services.update_instance": [
    { instance: "mysql/default", patch: { autostart: false } },
    {
      instance: "mysql/default",
      patch: { restart_policy: { on: "failure", max: 5, window_secs: 120 }, version: "8.4.14" },
    },
  ],
  "sites.add": [
    { dir: String.raw`C:\www\loja`, host: "loja.test", php: "8.3", server: "nginx", secure: true },
    { dir: SPACED, host: null, php: null, server: null, secure: false },
  ],
  "sites.proxy_add": [
    { host: "api.test", upstream: "http://127.0.0.1:3000", websocket: true, secure: null },
  ],
  "sites.remove": ["loja.test"],
  "sites.switch_php": [{ site: "loja.test", php: "8.4" }],
  "sites.open": [
    { site: "loja.test", with: "browser" },
    { site: "loja.test", with: "editor" },
    { site: "loja.test", with: "terminal" },
    { site: "loja.test", with: "explorer" },
  ],
  "sites.run_process": [{ site: "loja.test", process: "queue", action: "restart" }],
  "sites.set_driver": [
    { site: "loja.test", driver: "laravel" },
    { site: "loja.test", driver: null },
  ],
  "tools.install_spec": ["php@8.4"],
  "mail.mark_read": [
    { ids: ["abc"], read: true },
    { ids: ["abc", "def"], read: false },
  ],
  "mail.delete": [["abc"]],
  "mail.clear": [undefined],
  "mail.open": [undefined],
  "terminal.open": [undefined],
  "terminal.close": [{ id: "01J0000000000000000000000" }],
  "tools.use_version": [{ tool: "php", version: "8.4" }],
  "tools.remove_version": ["php@8.3.36"],
  "tools.rollback_series": ["php@8.3"],
  "tools.check_updates_one": ["php"],
  "tools.set_policy": [{ tool: "php", policy: "notify" }],
  "php.ext_enable": [{ version: "8.4", extension: "intl" }],
  "php.ext_disable": [{ version: "8.4", extension: "intl" }],
  "php.xdebug": [
    { version: "8.4", mode: "trigger" },
    { version: "8.3", mode: "on", port: 9010 },
    { version: "8.4", mode: "off" },
  ],
  "tools.install_specs": [{ specs: ["php@8.4.27", "nginx"], variant: "nts" }, { specs: ["redis"] }],
  "db.create": [
    { instance: "mysql/default", name: "loja", charset: "utf8mb4" },
    { instance: "postgresql/default", name: "loja_test", charset: null },
  ],
  "db.drop": [{ instance: "mysql/default", name: "loja" }],
  "db.export": [
    { instance: "mysql/default", name: "loja" },
    { instance: "mysql/default", name: "loja", output: String.raw`D:\Backups MySQL\loja.sql.gz` },
  ],
  "db.import": [
    { instance: "mysql/default", name: "loja", file: String.raw`C:\Meus Dumps\loja.sql.zst` },
  ],
  "db.shell": [
    { instance: "mysql/default", database: "loja" },
    { instance: "postgresql/default", database: null },
  ],
  "db.url": [{ instance: "mysql/default", database: "loja" }],
};

interface Entry {
  action: string;
  method: string;
  kind: string;
  samples: { input: unknown; command: string | null }[];
}

function generate(): Entry[] {
  return listActions()
    .map((action) => ({
      action: action.id,
      method: action.method,
      kind: action.kind,
      samples: (SAMPLES[action.id] ?? []).map((input) => ({
        input: input ?? null,
        command: (action as unknown as { cli(i: unknown): string | null }).cli(input),
      })),
    }))
    .sort((a, b) => a.action.localeCompare(b.action));
}

const serialize = (entries: Entry[]) => JSON.stringify(entries, null, 2) + "\n";

describe("gui-commands.json", () => {
  it("loads every action module of src/actions", () => {
    const names = Object.keys(actionModules).map((path) => path.replace(/^.*\//, ""));
    expect(names).toEqual(expect.arrayContaining(["databases.ts", "mail.ts", "sites.ts"]));
    expect(names.filter((name) => name.endsWith(".test.ts"))).toEqual([]);
  });

  it("has sample inputs for every registered action", () => {
    const missing = listActions()
      .map((a) => a.id)
      .filter((id) => (SAMPLES[id] ?? []).length === 0);
    expect(missing).toEqual([]);
    const known = new Set(listActions().map((a) => a.id));
    expect(Object.keys(SAMPLES).filter((id) => !known.has(id))).toEqual([]);
  });

  it("is regenerated identically (drift check)", () => {
    const text = serialize(generate());
    if (process.env.FORGE_PARITY_UPDATE === "1") writeFileSync(JSON_PATH, text);
    expect(readFileSync(JSON_PATH, "utf8").replace(/\r\n/g, "\n")).toBe(text);
  });

  it("renders a command for at least one sample of every action but terminal.open", () => {
    const silent = generate()
      .filter((e) => e.samples.every((s) => s.command === null))
      .map((e) => e.action);
    // `terminal.*` have an empty template (the equivalent is `forge shell`, docs/07 §8).
    expect(silent.filter((id) => !id.startsWith("terminal."))).toEqual([]);
  });
});

describe("Copiar comando", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  async function copyAndRead(command: string): Promise<string[]> {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(createElement(CliCommand, { command }));
    await userEvent.click(screen.getByRole("button"));
    await waitFor(() => {
      expect(writeText).toHaveBeenCalled();
    });
    return writeText.mock.calls.map((c) => c[0] as string);
  }

  it("writes exactly the rendered string, quoting a Windows path with spaces", async () => {
    const command = renderCli("sites.add", { dir: SPACED, host: "loja.test" });
    expect(command).toBe(
      String.raw`forge site add "C:\Meus Projetos\loja virtual" --host loja.test`,
    );
    expect(await copyAndRead(command ?? "")).toEqual([command]);
  });

  it("doubles the trailing backslash of a quoted path", async () => {
    const command = renderCli("sites.add", { dir: "D:\\meus sites\\" });
    expect(command).toBe('forge site add "D:\\meus sites\\\\"');
    expect(await copyAndRead(command ?? "")).toEqual([command]);
  });
});
