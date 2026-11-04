// M2-E1-T23: tool icons never render as black circles; a missing icon is a coloured monogram.
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  TOOL_TILE_CLASSES,
  ToolTile,
  isMonochromeSvg,
  toolColorIndex,
  toolMonogram,
  toolTileClass,
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
    const { container } = render(<ToolTile tool="mysql" size={24} />);
    const tile = container.querySelector("[data-tool-tile]");
    expect(tile).toHaveAttribute("data-tool-tile", "monogram");
    expect(tile).toHaveTextContent("My");
    expect(tile).toHaveClass(toolTileClass("mysql"), "text-fg-on-tool");
    expect(tile).toHaveStyle({ width: "24px", height: "24px" });
    expect(container.querySelector("img")).toBeNull();
  });

  it("whitens a monochrome icon on the coloured tile, still only as an <img>", () => {
    for (const svg of [PLAIN_CIRCLE, REGISTRY_LINE]) {
      const { container, unmount } = render(<ToolTile tool="redis" svg={svg} size={32} />);
      const tile = container.querySelector("[data-tool-tile]");
      expect(tile).toHaveAttribute("data-tool-tile", "tinted");
      expect(tile).toHaveClass(toolTileClass("redis"));
      const img = container.querySelector("img");
      expect(img?.getAttribute("src")).toMatch(/^data:image\/svg\+xml;charset=utf-8,/);
      expect(img).toHaveClass("brightness-0", "invert");
      expect(container.querySelector("svg")).toBeNull();
      unmount();
    }
  });

  it("shows a coloured icon as it is", () => {
    const { container } = render(<ToolTile tool="php" svg={COLOURED} size={48} />);
    expect(container.querySelector("[data-tool-tile]")).toHaveAttribute("data-tool-tile", "image");
    expect(container.querySelector("img")).not.toHaveClass("invert");
    expect(container.querySelector("svg")).toBeNull();
  });
});
