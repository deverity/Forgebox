import { describe, expect, it } from "vitest";
import type * as T from "../../ipc/generated";
import {
  charsetOptions,
  dsn,
  envSnippet,
  hasDumpExtension,
  importProblem,
  maskUrl,
  nameProblem,
  pickInstance,
  shownStrings,
  stderrTail,
  suggestDatabaseName,
} from "./model";

function inst(name: string, state: T.ServiceState): T.DbInstanceDto {
  return {
    instance_id: "01QAD8IN000000000000000001",
    instance: name,
    tool: name.split("/")[0] ?? name,
    version: "1.0.0",
    protocol: "mysql",
    state,
    port: null,
  };
}

describe("nameProblem", () => {
  it("follows the daemon's ^[A-Za-z0-9_]{1,64}$ and compares existing names exactly", () => {
    expect(nameProblem("", [])).toBe("required");
    expect(nameProblem("loja", [])).toBeUndefined();
    expect(nameProblem("Loja_2", ["loja"])).toBeUndefined();
    expect(nameProblem("loja", ["loja"])).toBe("exists");
    expect(nameProblem("a".repeat(64), [])).toBeUndefined();
    expect(nameProblem("a".repeat(65), [])).toBe("invalid");
    for (const bad of ["my db", "a-b", "loja;drop", "ação", "`x`", "a.b"]) {
      expect(nameProblem(bad, [])).toBe("invalid");
    }
  });
});

describe("helpers", () => {
  it("offers the engine default charset first", () => {
    expect(charsetOptions("mysql")[0]).toBe("utf8mb4");
    expect(charsetOptions("postgresql")[0]).toBe("UTF8");
  });

  it("derives the database name from a host", () => {
    expect(suggestDatabaseName("loja.test")).toBe("loja");
    expect(suggestDatabaseName("Minha-Loja.test")).toBe("minha_loja");
    expect(suggestDatabaseName("a.b.test")).toBe("a");
  });

  it("picks the chosen instance, else a running one, else the first", () => {
    const list = [inst("a/default", "stopped"), inst("b/default", "running")];
    expect(pickInstance(list, "a/default")?.instance).toBe("a/default");
    expect(pickInstance(list, null)?.instance).toBe("b/default");
    expect(pickInstance([inst("a/default", "stopped")], "zzz")?.instance).toBe("a/default");
    expect(pickInstance([], null)).toBeUndefined();
  });

  it("recognizes the three dump formats", () => {
    expect(hasDumpExtension("C:\\x\\Dump.SQL.ZST")).toBe(true);
    expect(hasDumpExtension("x.sql.gz")).toBe(true);
    expect(hasDumpExtension("x.sql")).toBe(true);
    expect(hasDumpExtension("x.zip")).toBe(false);
  });

  it("restricts a remote import to www/", () => {
    expect(importProblem("", false)).toBe("required");
    expect(importProblem("C:/x.zip", false)).toBe("badExtension");
    expect(importProblem("D:/dumps/x.sql", false)).toBeUndefined();
    expect(importProblem("D:/dumps/x.sql", true)).toBe("remoteOutside");
    expect(importProblem("C:\\Forgebox\\WWW\\d\\x.sql", true)).toBeUndefined();
  });

  it("keeps the last stderr lines after the first line of the error", () => {
    expect(stderrTail("client failed (exit 1):\nerr a\nerr b\n")).toBe("err a\nerr b");
    expect(stderrTail("only one line")).toBe("");
    const many = ["head", ...Array.from({ length: 30 }, (_, i) => `l${String(i)}`)].join("\n");
    expect(stderrTail(many).split("\n")).toHaveLength(20);
    expect(stderrTail(many).split("\n")[19]).toBe("l29");
  });
});

describe("connection strings", () => {
  const result: T.DbUrlDto = {
    url: "postgresql://postgres:p%40ss@127.0.0.1:5432/loja",
    env: {
      vars: {
        DB_HOST: "127.0.0.1",
        DB_PORT: "5432",
        DB_DATABASE: "loja",
        DB_PASSWORD: "p@ss",
      },
    },
  };

  it("masks the password of a URL and leaves an empty one alone", () => {
    expect(maskUrl("mysql://root:s3@127.0.0.1:3306/x")).toBe(
      "mysql://root:••••••••@127.0.0.1:3306/x",
    );
    expect(maskUrl("mysql://root@127.0.0.1:3306/x")).toBe("mysql://root@127.0.0.1:3306/x");
    expect(maskUrl("mysql://root:@127.0.0.1:3306/x")).toBe("mysql://root:@127.0.0.1:3306/x");
  });

  it("masks secret .env keys, sorted, until revealed", () => {
    expect(envSnippet(result.env.vars, true)).toBe(
      "DB_DATABASE=loja\nDB_HOST=127.0.0.1\nDB_PASSWORD=••••••••\nDB_PORT=5432",
    );
    expect(envSnippet(result.env.vars, false)).toContain("DB_PASSWORD=p@ss");
  });

  it("builds a PDO DSN per protocol", () => {
    expect(dsn("postgresql", result.env.vars)).toBe("pgsql:host=127.0.0.1;port=5432;dbname=loja");
    expect(dsn("mysql", { DB_HOST: "h", DB_DATABASE: "d" })).toBe("mysql:host=h;dbname=d");
    expect(dsn("mysql", {})).toBeNull();
  });

  it("shows no password anywhere while masked", () => {
    const shown = shownStrings("postgresql", result, false);
    expect(JSON.stringify(shown)).not.toContain("p%40ss");
    expect(JSON.stringify(shown)).not.toContain("p@ss");
    expect(shownStrings("postgresql", result, true).url).toBe(result.url);
  });
});
