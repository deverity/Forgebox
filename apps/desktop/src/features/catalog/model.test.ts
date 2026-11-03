import { describe, expect, it } from "vitest";
import type {
  CatalogEntryDto,
  ToolPlanDto,
  ToolReleaseDto,
  VariantDefDto,
} from "../../ipc/generated";
import {
  categoriesOf,
  defaultRelease,
  groupBySeries,
  installSpec,
  matchesFilters,
  planSpecs,
  splitStacks,
  summarizePlan,
  variantChoice,
} from "./model";

function entry(id: string, patch: Partial<CatalogEntryDto> = {}): CatalogEntryDto {
  return {
    id,
    name: id,
    category: "runtime",
    description: null,
    tags: [],
    registry: "official",
    versions: [],
    installed: false,
    ...patch,
  };
}

function release(version: string, patch: Partial<ToolReleaseDto> = {}): ToolReleaseDto {
  return { version, series: version.split(".").slice(0, 2).join("."), ...patch };
}

const TS: VariantDefDto = {
  key: "ts",
  values: ["nts", "ts"],
  default: "nts",
  labels: { nts: "Non Thread Safe" },
};

describe("filters", () => {
  const list = [
    entry("a", { installed: true }),
    entry("b", { category: "database", update_available: "2.0" }),
    entry("c", { kind: "stack", category: "stack" }),
  ];

  it("filters by installed, updates and category", () => {
    const pick = (installed: "all" | "installed" | "updates", category: string | null) =>
      list.filter((e) => matchesFilters(e, { installed, category })).map((e) => e.id);
    expect(pick("all", null)).toEqual(["a", "b", "c"]);
    expect(pick("installed", null)).toEqual(["a"]);
    expect(pick("updates", null)).toEqual(["b"]);
    expect(pick("all", "database")).toEqual(["b"]);
    expect(pick("installed", "database")).toEqual([]);
  });

  it("lists categories sorted and splits stacks", () => {
    expect(categoriesOf(list)).toEqual(["database", "runtime", "stack"]);
    const { stacks, tools } = splitStacks(list);
    expect(stacks.map((e) => e.id)).toEqual(["c"]);
    expect(tools.map((e) => e.id)).toEqual(["a", "b"]);
  });
});

describe("releases", () => {
  const releases = [release("8.4.27"), release("8.4.26"), release("8.3.40", { installed: true })];

  it("groups by series keeping the newest first", () => {
    const groups = groupBySeries(releases);
    expect(groups.map((g) => g.series)).toEqual(["8.4", "8.3"]);
    expect(groups[0]?.releases.map((r) => r.version)).toEqual(["8.4.27", "8.4.26"]);
  });

  it("defaults to the newest release not installed yet", () => {
    expect(defaultRelease(releases)?.version).toBe("8.4.27");
    expect(defaultRelease([release("8.3.40", { installed: true })])?.version).toBe("8.3.40");
    expect(defaultRelease([])).toBeUndefined();
  });

  it("builds the exact spec", () => {
    expect(installSpec("php", "8.4.27")).toBe("php@8.4.27");
  });

  it("offers the variant dimension only with more than one build value", () => {
    const both = release("8.4.27", {
      variants: [
        { variant: { ts: "nts" }, canonical_name: "a" },
        { variant: { ts: "ts" }, canonical_name: "b" },
      ],
    });
    const choice = variantChoice([TS], both);
    expect(choice?.key).toBe("ts");
    expect(choice?.values).toEqual(["nts", "ts"]);
    expect(choice?.default).toBe("nts");
    expect(choice?.labels.nts).toBe("Non Thread Safe");

    const single = release("8.4.27", {
      variants: [{ variant: { ts: "nts" }, canonical_name: "a" }],
    });
    expect(variantChoice([TS], single)).toBeNull();
    expect(variantChoice([{ ...TS, derived: true }], both)).toBeNull();
    expect(variantChoice([TS], undefined)).toBeNull();
  });
});

describe("plan", () => {
  const plan: ToolPlanDto = {
    items: [
      { tool: "php", spec: "php@8.4.27", version: "8.4.27", size_bytes: 10, reason: "requested" },
      {
        tool: "mysql",
        spec: "mysql@8.4.3",
        version: "8.4.3",
        size_bytes: 20,
        size_estimated: true,
        reason: "requested",
      },
      {
        tool: "redis",
        spec: "redis@7.4.1",
        version: "7.4.1",
        already_installed: true,
        reason: "requested",
      },
    ],
    total_bytes: 30,
    unknown_size: 1,
  };

  it("summarizes what downloads, what is present and whether the total is approximate", () => {
    const summary = summarizePlan(plan);
    expect(summary.download.map((i) => i.tool)).toEqual(["php", "mysql"]);
    expect(summary.present.map((i) => i.tool)).toEqual(["redis"]);
    expect(summary.totalBytes).toBe(30);
    expect(summary.approximate).toBe(true);
    expect(summary.unknownSize).toBe(1);
  });

  it("passes only the specs that still need a download", () => {
    expect(planSpecs(plan)).toEqual(["php@8.4.27", "mysql@8.4.3"]);
  });
});
