// The official registry's icons and brand colours (M2-E1-T24), bundled into the QA build and
// the vitest suites so the fake daemon answers `tools.icon` / `tools.search` with what the real
// one serves: the official logo glyph (`registry/icons/*.svg`) and `metadata.icon_color` of the
// manifests in `registry/tools` and `registry/stacks`. Never part of the production bundle (the
// QA bootstrap is only imported in `--mode qa`).

const SVGS = import.meta.glob<string>("../../../../../registry/icons/*.svg", {
  query: "?raw",
  import: "default",
  eager: true,
});

const MANIFESTS = import.meta.glob<string>("../../../../../registry/{tools,stacks}/*.yaml", {
  query: "?raw",
  import: "default",
  eager: true,
});

export interface RegistryIcon {
  /** The SVG document of `metadata.icon`. */
  svg: string;
  /** `metadata.icon_color` (`#RRGGBB`), or `null` without one. */
  color: string | null;
}

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** A top-level `metadata` field of a manifest (`  icon: php.svg`, `  icon_color: "#777BB4"`). */
function field(text: string, name: string): string | null {
  const match = new RegExp(`^  ${name}:\\s*"?([^"\\s]+)"?\\s*$`, "m").exec(text);
  return match?.[1] ?? null;
}

function build(): ReadonlyMap<string, RegistryIcon> {
  const svgs = new Map(Object.entries(SVGS).map(([path, svg]) => [fileName(path), svg]));
  const icons = new Map<string, RegistryIcon>();
  for (const text of Object.values(MANIFESTS)) {
    const id = field(text, "id");
    const icon = field(text, "icon");
    const svg = icon === null ? undefined : svgs.get(icon);
    if (id === null || svg === undefined) continue;
    icons.set(id, { svg, color: field(text, "icon_color") });
  }
  return icons;
}

/** Icon and brand colour of every official tool and stack, by id. */
export const REGISTRY_ICONS: ReadonlyMap<string, RegistryIcon> = build();

/** The registry icon of `id`, if the official registry has one. */
export function registryIcon(id: string): RegistryIcon | undefined {
  return REGISTRY_ICONS.get(id);
}
