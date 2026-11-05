import { describe, expect, it } from "vitest";
import { addProxy, addSite } from "../../actions/sites";
import { site } from "../../../qa/fixtures/scenarios/data";
import { ForgeApiError } from "../../ipc/errors";
import {
  fieldOfInvalidInput,
  filterSites,
  folderName,
  hostProblem,
  indexSites,
  normalizeHost,
  suggestHost,
  upstreamProblem,
} from "./model";

const byId = (...sites: ReturnType<typeof site>[]) =>
  Object.fromEntries(sites.map((s) => [s.id, s]));

describe("search", () => {
  const index = indexSites(
    byId(
      site(1, "loja.test", { driver: "laravel", root: "C:/www/Loja Virtual" }),
      site(2, "blog.test", { aliases: ["journal.test"], driver: "wordpress" }),
      site(3, "api.test", { driver: null, root: null }),
    ),
  );

  it("sorts by host and matches host, alias, root and driver without case or accents", () => {
    expect(index.map((e) => e.site.host)).toEqual(["api.test", "blog.test", "loja.test"]);
    const hosts = (q: string) => filterSites(index, q).map((e) => e.site.host);
    expect(hosts("LOJA")).toEqual(["loja.test"]);
    expect(hosts("journal")).toEqual(["blog.test"]);
    expect(hosts("virtual")).toEqual(["loja.test"]);
    expect(hosts("wordpress")).toEqual(["blog.test"]);
    expect(hosts("loja laravel")).toEqual(["loja.test"]);
    expect(hosts("loja wordpress")).toEqual([]);
    expect(hosts("  ")).toHaveLength(3);
  });
});

describe("host suggestion", () => {
  it("builds <folder>.<tld> in lowercase from Windows and POSIX paths", () => {
    expect(folderName("C:\\Meus Projetos\\Loja Nova\\")).toBe("Loja Nova");
    expect(suggestHost("C:\\Meus Projetos\\Loja Nova", "test")).toBe("loja-nova.test");
    expect(suggestHost("/srv/www/Café_Central", ".test")).toBe("cafe-central.test");
    expect(suggestHost("", "test")).toBe("");
  });

  it("lowercases a typed host and reports it", () => {
    expect(normalizeHost("Loja.Test")).toEqual({ host: "loja.test", lowercased: true });
    expect(normalizeHost(" loja.test ")).toEqual({ host: "loja.test", lowercased: false });
  });

  it("validates host and upstream shape", () => {
    expect(hostProblem("")).toBe("empty");
    expect(hostProblem("loja")).toBe("syntax");
    expect(hostProblem("loja_x.test")).toBe("syntax");
    expect(hostProblem("loja.test")).toBeNull();
    expect(upstreamProblem("")).toBe("empty");
    expect(upstreamProblem("localhost:3000")).toBe("syntax");
    expect(upstreamProblem("ftp://x")).toBe("syntax");
    expect(upstreamProblem("http://127.0.0.1:3000")).toBeNull();
  });
});

describe("invalid_input mapping", () => {
  const input = (details: unknown, message = "bad") =>
    new ForgeApiError({ code: "invalid_input", message, details });

  it("prefers the field the daemon names", () => {
    expect(fieldOfInvalidInput(input({ reason: "x", args: { field: "upstream" } }), "host")).toBe(
      "upstream",
    );
  });

  it("guesses from the reason and falls back to the given field", () => {
    expect(fieldOfInvalidInput(input({ reason: "host_tld", args: {} }), "dir")).toBe("host");
    expect(fieldOfInvalidInput(input({ reason: "dir_missing", args: {} }), "host")).toBe("dir");
    expect(fieldOfInvalidInput(input({ reason: "other", args: {} }, "nope"), "host")).toBe("host");
  });

  it("ignores other codes", () => {
    const other = new ForgeApiError({ code: "conflict", message: "x" });
    expect(fieldOfInvalidInput(other, "host")).toBeNull();
  });
});

describe("equivalent forge commands", () => {
  it("renders site add with a quoted path", () => {
    expect(
      addSite.cli({
        dir: "C:\\Meus Projetos\\loja",
        host: "loja.test",
        php: "8.4",
        server: "nginx",
        secure: true,
      }),
    ).toBe(
      'forge site add "C:\\Meus Projetos\\loja" --host loja.test --php 8.4 --server nginx --secure',
    );
  });

  it("renders proxy add with the websocket flag", () => {
    expect(
      addProxy.cli({ host: "app.test", upstream: "http://127.0.0.1:3000", websocket: true }),
    ).toBe("forge proxy add app.test http://127.0.0.1:3000 --ws");
  });
});
