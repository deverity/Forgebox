import { describe, expect, it } from "vitest";
import type { LogLineDto } from "../../ipc/generated";
import {
  LineMerger,
  NO_FILTERS,
  accessHosts,
  applyFilters,
  cliHint,
  findFileLinks,
  levelOf,
  splitText,
} from "./model";

function line(
  seq: number,
  ts: string | null,
  text = `line ${String(seq)}`,
  extra = {},
): LogLineDto {
  return { seq, ts, source: "stdout", level: null, text, ...extra };
}

const at = (second: number) => `2026-01-01T10:00:${String(second).padStart(2, "0")}Z`;
const texts = (merger: LineMerger) =>
  merger.rows.map((row) => (row.kind === "line" ? row.line.text : `gap:${String(row.missed)}`));

describe("merging streams", () => {
  it("orders three streams with interleaved timestamps by ts", () => {
    const merger = new LineMerger();
    // Each stream is in order on its own; arrival is scrambled across them.
    merger.push("a", [line(1, at(1), "a1"), line(2, at(4), "a2"), line(3, at(7), "a3")]);
    merger.push("c", [line(1, at(3), "c1"), line(2, at(6), "c2"), line(3, at(9), "c3")]);
    merger.push("b", [line(1, at(2), "b1"), line(2, at(5), "b2"), line(3, at(8), "b3")]);
    expect(texts(merger)).toEqual(["a1", "b1", "c1", "a2", "b2", "c2", "a3", "b3", "c3"]);
    // A late arrival slots into place instead of going last.
    merger.push("b", [line(4, at(4), "b-late")]);
    const order = texts(merger);
    expect(order.indexOf("b-late")).toBeLessThan(order.indexOf("b2"));
    expect(order.indexOf("b-late")).toBeGreaterThan(order.indexOf("c1"));
  });

  it("keeps arrival order for lines without ts", () => {
    const merger = new LineMerger();
    merger.push("a", [line(1, null, "a1"), line(2, null, "a2")]);
    merger.push("b", [line(1, null, "b1")]);
    merger.push("a", [line(3, null, "a3")]);
    merger.push("b", [line(2, null, "b2")]);
    expect(texts(merger)).toEqual(["a1", "a2", "b1", "a3", "b2"]);
  });

  it("puts a line without ts after the previous line of its own stream", () => {
    const merger = new LineMerger();
    merger.push("a", [line(1, at(5), "a1"), line(2, null, "a2-no-ts")]);
    merger.push("b", [line(1, at(3), "b1"), line(2, at(8), "b2")]);
    expect(texts(merger)).toEqual(["b1", "a1", "a2-no-ts", "b2"]);
  });

  it("skips lines it already has (replays after a reconnection)", () => {
    const merger = new LineMerger();
    merger.push("a", [line(1, null), line(2, null)]);
    expect(merger.push("a", [line(2, null), line(3, null)])).toBe(1);
    expect(merger.size).toBe(3);
  });

  it("keeps at most the cap, dropping the oldest", () => {
    const merger = new LineMerger(100);
    for (let n = 0; n < 5; n += 1) {
      merger.push(
        "a",
        Array.from({ length: 50 }, (_, i) => line(n * 50 + i + 1, null)),
      );
    }
    expect(merger.size).toBe(100);
    expect(merger.oldestSeq("a")).toBe(151);
  });

  it("marks lost lines where the stream stands", () => {
    const merger = new LineMerger();
    merger.push("a", [line(1, at(1), "a1")]);
    merger.gap("a", 12);
    merger.push("a", [line(9, at(2), "a9")]);
    expect(texts(merger)).toEqual(["a1", "gap:12", "a9"]);
  });

  it("prepends older lines in front of the target's own (before_seq)", () => {
    const merger = new LineMerger();
    merger.push("a", [line(201, at(20), "a201"), line(202, at(21), "a202")]);
    merger.push("b", [line(1, at(25), "b1")]);
    const added = merger.prepend("a", [
      line(199, at(18), "a199"),
      line(200, at(19), "a200"),
      line(201, at(20), "a201"),
    ]);
    expect(added).toBe(2);
    expect(texts(merger)).toEqual(["a199", "a200", "a201", "a202", "b1"]);
    expect(merger.oldestSeq("a")).toBe(199);
  });

  it("forgets a target that was unselected", () => {
    const merger = new LineMerger();
    merger.push("a", [line(1, null)]);
    merger.push("b", [line(1, null)]);
    merger.remove("a");
    expect(merger.rows.map((row) => row.target)).toEqual(["b"]);
  });
});

describe("filters", () => {
  const merger = new LineMerger();
  merger.push("a", [
    { ...line(1, at(1), "Upstream timed out"), level: "error" },
    { ...line(2, at(2), "all good"), level: "info" },
    { ...line(3, at(3), "slow"), level: "warning" },
    line(4, null, "no level"),
  ]);
  merger.push("site:loja.test", [
    line(1, at(4), "GET /a 200", {
      access: { host: "loja.test", status: 200, method: "GET", uri: "/a" },
    }),
    line(2, at(5), "GET /b 200", {
      access: { host: "blog.test", status: 200, method: "GET", uri: "/b" },
    }),
  ]);
  merger.gap("a", 3);
  const ids = (filters: Partial<typeof NO_FILTERS>, now = Date.parse(at(30))) =>
    applyFilters(merger.rows, { ...NO_FILTERS, ...filters }, now).map((row) =>
      row.kind === "line" ? row.line.text : "gap",
    );

  it("matches the text without caring about case", () => {
    expect(ids({ text: "UPSTREAM" })).toEqual(["Upstream timed out", "gap"]);
  });

  it("filters by level, folding warning into warn", () => {
    expect(ids({ level: "warn" })).toEqual(["slow", "gap"]);
    expect(ids({ level: "error" })).toEqual(["Upstream timed out", "gap"]);
    expect(levelOf(line(1, null, "x", { level: "notice" }))).toBe("info");
  });

  it("filters by site and lists the hosts", () => {
    expect(ids({ site: "blog.test" })).toEqual(["gap", "GET /b 200"]);
    expect(accessHosts(merger.rows)).toEqual(["blog.test", "loja.test"]);
  });

  it("filters by since, keeping lines without ts", () => {
    // 5 minutes and 0.5 s after 10:00:01: only the line at :01 is older than the window.
    const now = Date.parse(at(1)) + 5 * 60_000 + 500;
    expect(ids({ since: "5m" }, now)).toEqual([
      "all good",
      "slow",
      "no level",
      "gap",
      "GET /a 200",
      "GET /b 200",
    ]);
  });

  it("never hides the lost-lines marker", () => {
    expect(ids({ text: "zzz" })).toEqual(["gap"]);
  });
});

describe("stack-trace links", () => {
  it("finds the frames of a PHP 8.4 fatal error", () => {
    const text =
      "PHP Fatal error:  Uncaught Exception: boom in C:\\Forgebox\\www\\loja\\public\\index.php:12";
    expect(findFileLinks(text)[0]?.link).toEqual({
      path: "C:\\Forgebox\\www\\loja\\public\\index.php",
      line: 12,
    });
    const frame = "#0 C:\\Forgebox\\www\\loja\\src\\Cart.php(87): Cart->total()";
    expect(findFileLinks(frame)[0]?.link).toEqual({
      path: "C:\\Forgebox\\www\\loja\\src\\Cart.php",
      line: 87,
    });
    const warning = "PHP Warning:  Undefined variable $x in /var/www/a.php on line 9";
    expect(findFileLinks(warning)[0]?.link).toEqual({ path: "/var/www/a.php", line: 9 });
  });

  it("finds the frames of a Laravel trace", () => {
    const windows =
      "#11 C:\\www\\loja\\vendor\\laravel\\framework\\src\\Illuminate\\Pipeline\\Pipeline.php(180): Illuminate\\Pipeline\\Pipeline->handle()";
    const posix =
      "#12 /var/www/loja/vendor/laravel/framework/src/Illuminate/Routing/Route.php(262): Route->runController()";
    const message =
      "[2026-01-01 10:00:00] local.ERROR: Class not found at /var/www/loja/app/Http/A.php:31";
    expect(findFileLinks(windows)[0]?.link).toEqual({
      path: "C:\\www\\loja\\vendor\\laravel\\framework\\src\\Illuminate\\Pipeline\\Pipeline.php",
      line: 180,
    });
    expect(findFileLinks(posix)[0]?.link).toEqual({
      path: "/var/www/loja/vendor/laravel/framework/src/Illuminate/Routing/Route.php",
      line: 262,
    });
    expect(findFileLinks(message)[0]?.link).toEqual({
      path: "/var/www/loja/app/Http/A.php",
      line: 31,
    });
    expect(findFileLinks("nothing to open here")).toEqual([]);
  });

  it("splits a line into plain, highlighted and link parts", () => {
    const parts = splitText("#0 /var/www/a.php(5): run() failed", "run", true);
    expect(parts.map((part) => [part.text, part.link !== undefined, part.match === true])).toEqual([
      ["#0 ", false, false],
      ["/var/www/a.php(5)", true, false],
      [": ", false, false],
      ["run", false, true],
      ["() failed", false, false],
    ]);
  });
});

describe("command line", () => {
  it("renders the same stream as forge logs", () => {
    expect(cliHint("nginx.error", "upstream")).toBe("forge logs nginx.error -f --grep upstream");
    expect(cliHint("mysql/default", "")).toBe("forge logs mysql/default -f");
  });
});
