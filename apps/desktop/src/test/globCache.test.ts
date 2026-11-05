import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const src = join(dirname(fileURLToPath(import.meta.url)), "..");

// Vitest 5 caches transformed modules on disk (fsModuleCache, on by default) keyed by the
// module's own source. A module that uses import.meta.glob depends on other files, so Vitest
// skips it, but only when the source contains the literal `import.meta.glob(`. A type argument
// (`import.meta.glob<T>(`) defeats that check: the cached file list goes stale and files added
// later (a new i18n namespace, a new feature slots.ts) silently vanish from the tests.
// Type the binding instead: `const x: Record<string, T> = import.meta.glob("...")`.
const TYPED_GLOB = /import\.meta\.glob\s*</;

function sources(): string[] {
  return readdirSync(src, { recursive: true, encoding: "utf8" })
    .filter((file) => /\.tsx?$/.test(file) && !/\.test\.tsx?$/.test(file))
    .map((file) => file.replaceAll("\\", "/"));
}

describe("import.meta.glob and the Vitest module cache", () => {
  it("never passes a type argument to import.meta.glob", () => {
    const offenders = sources().filter((file) =>
      TYPED_GLOB.test(readFileSync(join(src, file), "utf8")),
    );
    expect(offenders).toEqual([]);
  });

  it("still finds the known glob users (the scan is not vacuous)", () => {
    const users = sources().filter((file) =>
      readFileSync(join(src, file), "utf8").includes("import.meta.glob("),
    );
    expect(users).toEqual(expect.arrayContaining(["app/slotModules.ts", "i18n/index.ts"]));
  });
});
