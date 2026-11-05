// M2-E1-T23: tool icons never render as black circles; a missing icon is a coloured monogram.
// M2-E1-T24: the official logo is a white glyph on its brand colour (style C), >= 3:1.
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { REGISTRY_ICONS } from "../../../qa/fixtures/scenarios/registryIcons";
import { luminance } from "../../app/appearance";
import { toolTile } from "../tokens";
import {
  GLYPH_MIN_CONTRAST,
  TOOL_TILE_CLASSES,
  ToolTile,
  brandTile,
  isMonochromeSvg,
  toolColorIndex,
  toolMonogram,
  toolTileClass,
  toolTileMetrics,
  whiteContrast,
} from "./ToolTile";

// The QA fixture icon (a circle with no paint: black by default) and the registry's line icons.
const PLAIN_CIRCLE =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3"/></svg>';
const REGISTRY_LINE =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect x="4" y="4" width="56" height="56" rx="14" fill="none" stroke="currentColor" stroke-width="4"/><text fill="currentColor">M</text></svg>';
const COLOURED =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><circle r="3" fill="var(--x)" style="fill: rebeccapurple"/></svg>';

describe("tool colour and monogram", () => {
  it("picks the same token for the same id, spread over the palette", () => {
    const ids = ["mysql", "redis", "mailpit", "php", "nginx", "apache", "nodejs", "postgresql"];
    for (const id of ids) {
      expect(toolColorIndex(id)).toBe(toolColorIndex(id));
      expect(TOOL_TILE_CLASSES).toContain(toolTileClass(id));
    }
    expect(new Set(ids.map(toolColorIndex)).size).toBeGreaterThanOrEqual(4);
  });

  it("spells two letters like the mockup", () => {
    expect(toolMonogram("mysql")).toBe("My");
    expect(toolMonogram("redis")).toBe("Rd");
    expect(toolMonogram("php-fpm")).toBe("Pf");
    expect(toolMonogram("x")).toBe("X");
    expect(toolMonogram("")).toBe("?");
  });

  it("tells monochrome icons (currentColor, black, no paint) from coloured ones", () => {
    expect(isMonochromeSvg(PLAIN_CIRCLE)).toBe(true);
    expect(isMonochromeSvg(REGISTRY_LINE)).toBe(true);
    expect(isMonochromeSvg("<svg/>")).toBe(true);
    expect(isMonochromeSvg(COLOURED)).toBe(false);
  });
});

describe("ToolTile", () => {
  it("without an icon shows a coloured monogram tile, not a black circle", () => {
    const { container } = render(<ToolTile tool="mysql" color="#4479A1" variant="list" />);
    const tile = container.querySelector("[data-tool-tile]");
    expect(tile).toHaveAttribute("data-tool-tile", "monogram");
    expect(tile).toHaveTextContent("My");
    expect(tile).toHaveClass(toolTileClass("mysql"), "text-fg-on-tool");
    expect(tile).toHaveStyle({ width: "30px", height: "30px", borderRadius: "8px" });
    expect(container.querySelector("img")).toBeNull();
  });

  it("keeps the monogram for an icon without a (valid) brand colour: third-party registries", () => {
    for (const color of [undefined, null, "", "#F00", "purple", "#777BB4FF"]) {
      const { container, unmount } = render(
        <ToolTile tool="redis" svg={PLAIN_CIRCLE} color={color} variant="grid" />,
      );
      const tile = container.querySelector("[data-tool-tile]");
      expect(tile, String(color)).toHaveAttribute("data-tool-tile", "monogram");
      expect(tile).toHaveTextContent("Rd");
      expect(tile).toHaveClass(toolTileClass("redis"));
      expect(container.querySelector("img")).toBeNull();
      unmount();
    }
  });

  it("draws the monochrome logo white on the brand-colour tile, still only as an <img>", () => {
    for (const svg of [PLAIN_CIRCLE, REGISTRY_LINE]) {
      const { container, unmount } = render(
        <ToolTile tool="php" svg={svg} color="#777BB4" variant="grid" />,
      );
      const tile = container.querySelector<HTMLElement>("[data-tool-tile]");
      expect(tile).toHaveAttribute("data-tool-tile", "brand");
      expect(tile?.style.getPropertyValue("--tool-brand")).toBe("#777BB4");
      expect(tile).toHaveClass("bg-(--tool-brand)");
      expect(tile?.className).toContain("var(--tool-tile-stroke)");
      expect(tile).not.toHaveAttribute("data-tool-edge");
      expect(tile).toHaveStyle({ width: "40px", height: "40px", borderRadius: "10px" });
      // No palette class: the colour is the brand's.
      for (const palette of TOOL_TILE_CLASSES) expect(tile).not.toHaveClass(palette);
      const img = container.querySelector("img");
      expect(img?.getAttribute("src")).toMatch(/^data:image\/svg\+xml;charset=utf-8,/);
      expect(img).toHaveClass("brightness-0", "invert");
      expect(img).toHaveStyle({ width: "24px", height: "24px" });
      expect(container.querySelector("svg")).toBeNull();
      unmount();
    }
  });

  it("gives very dark tiles the light edge (MariaDB, Next.js)", () => {
    for (const color of ["#003545", "#000000", "#080606"]) {
      const { container, unmount } = render(
        <ToolTile tool="x" svg={PLAIN_CIRCLE} color={color} variant="list" />,
      );
      const tile = container.querySelector("[data-tool-tile]");
      expect(tile).toHaveAttribute("data-tool-edge", "dark");
      expect(tile?.className).toContain("var(--tool-tile-edge)");
      unmount();
    }
  });

  it("uses the tool-tile tokens for every variant", () => {
    for (const variant of ["list", "grid", "hero"] as const) {
      expect(toolTileMetrics(variant)).toEqual({
        size: toolTile[`${variant}-size`],
        glyph: toolTile[`${variant}-glyph`],
        radius: toolTile[`${variant}-radius`],
      });
    }
    expect(toolTileMetrics("list")).toEqual({ size: 30, glyph: 19, radius: 8 });
    expect(toolTileMetrics("grid")).toEqual({ size: 40, glyph: 24, radius: 10 });
  });

  it("shows a coloured icon as it is", () => {
    const { container } = render(<ToolTile tool="php" svg={COLOURED} variant="hero" />);
    expect(container.querySelector("[data-tool-tile]")).toHaveAttribute("data-tool-tile", "image");
    expect(container.querySelector("img")).not.toHaveClass("invert");
    expect(container.querySelector("svg")).toBeNull();
  });
});

describe("brand tile colour", () => {
  it("keeps a colour that already gives white 3:1 and darkens a light one generically", () => {
    expect(brandTile("#777BB4")).toEqual({ fill: "#777BB4", dark: false });
    expect(brandTile("#ff2d20")).toEqual({ fill: "#FF2D20", dark: false });
    const light = brandTile("#00B786");
    expect(light?.fill).not.toBe("#00B786");
    expect(whiteContrast(light?.fill ?? "")).toBeGreaterThanOrEqual(GLYPH_MIN_CONTRAST);
    // Darkened, not shifted: the same hue, lower luminance.
    expect(luminance(light?.fill ?? "")).toBeLessThan(luminance("#00B786"));
    // Even white and yellow end up with a usable tile.
    for (const color of ["#FFFFFF", "#FFFF00", "#ECD53F"]) {
      expect(whiteContrast(brandTile(color)?.fill ?? "")).toBeGreaterThanOrEqual(3);
    }
    expect(brandTile("#12345")).toBeNull();
    expect(brandTile(undefined)).toBeNull();
  });

  // Every icon_color of the official registry (registry/tools, registry/stacks).
  const registry = resolve(import.meta.dirname, "../../../../../registry");
  const colours = ["tools", "stacks"].flatMap((folder) =>
    readdirSync(join(registry, folder))
      .filter((name) => name.endsWith(".yaml"))
      .map((name) => {
        const text = readFileSync(join(registry, folder, name), "utf8");
        const color = /^ {2}icon_color:\s*"(#[0-9A-Fa-f]{6})"\s*$/m.exec(text)?.[1];
        return [`${folder}/${name}`, color] as const;
      }),
  );

  it("every registry manifest has a brand colour", () => {
    expect(colours.length).toBe(17);
    for (const [file, color] of colours) expect(color, file).toMatch(/^#[0-9A-F]{6}$/);
  });

  it.each(colours)("%s: the white glyph reaches 3:1 on its (possibly darkened) tile", (_, c) => {
    const tile = brandTile(c);
    expect(tile).not.toBeNull();
    expect(whiteContrast(tile?.fill ?? "")).toBeGreaterThanOrEqual(GLYPH_MIN_CONTRAST);
  });

  it("the QA fixtures serve every registry icon with its colour", () => {
    expect(REGISTRY_ICONS.size).toBe(16);
    for (const [id, icon] of REGISTRY_ICONS) {
      expect(icon.svg, id).toContain('fill="currentColor"');
      expect(isMonochromeSvg(icon.svg), id).toBe(true);
      expect(icon.color, id).toMatch(/^#[0-9A-F]{6}$/);
    }
  });
});
