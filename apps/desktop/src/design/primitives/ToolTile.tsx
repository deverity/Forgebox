import type { CSSProperties } from "react";
import { luminance, mixHex } from "../../app/appearance";
import { toolTile } from "../tokens";
import { cx } from "./shared";

// The icon of a tool on every screen that shows one (M2-E1-T24, docs/07 section 2.4, style C of
// the approved icons mockup):
// - the registry icon is the official logo as a monochrome glyph and the manifest carries the
//   brand colour (`metadata.icon_color`): the glyph is drawn WHITE on a rounded tile filled with
//   that colour (darkened when white would fall under 3:1), with a thin inner stroke;
// - a full-colour SVG (third-party registries) is shown as it is;
// - without an icon or without a brand colour, a rounded-square monogram ("My", "Rd") on one of
//   the `tool-*` palette tokens (picked from the id) holds the same box.
// The SVG is ONLY ever an <img> with a data URI: a document from a manifest never enters the
// DOM, so its scripts, handlers and references cannot run (docs/07 section 7, docs/11).

const DATA_URI = "data:image/svg+xml;charset=utf-8,";

/** The `tool-*` token classes (literal, so Tailwind generates them). */
export const TOOL_TILE_CLASSES = [
  "bg-tool-1",
  "bg-tool-2",
  "bg-tool-3",
  "bg-tool-4",
  "bg-tool-5",
  "bg-tool-6",
  "bg-tool-7",
  "bg-tool-8",
] as const;

/** FNV-1a (32 bit) of the id: the same tool always gets the same tile colour. */
export function toolColorIndex(tool: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < tool.length; i += 1) {
    hash ^= tool.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash % TOOL_TILE_CLASSES.length;
}

/** The token class of the tile of `tool`. */
export function toolTileClass(tool: string): string {
  return TOOL_TILE_CLASSES[toolColorIndex(tool)] ?? "bg-tool-1";
}

const VOWELS = new Set(["a", "e", "i", "o", "u"]);

/**
 * Two letters for a tool id: the initials of the first two parts (`php-fpm` -> "Pf"), else the
 * first letter and the next consonant (`mysql` -> "My", `redis` -> "Rd"); one letter if that is
 * all there is.
 */
export function toolMonogram(tool: string): string {
  const parts = tool
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter((p) => p.length > 0);
  const first = parts[0] ?? "";
  if (first === "") return "?";
  const head = first.charAt(0).toUpperCase();
  const second = parts[1];
  if (second !== undefined) return head + second.charAt(0);
  // Ids are ASCII (`[a-z0-9]` after the split above): code units are characters.
  const rest = first.slice(1).split("");
  const next = rest.find((c) => !VOWELS.has(c)) ?? rest[0];
  return next === undefined ? head : head + next;
}

// Paint attributes and properties of an SVG and the values that mean "no colour of its own".
const PAINT =
  /(?:fill|stroke|stop-color|flood-color|lighting-color|color)\s*[=:]\s*["']?\s*([^"';)\s>]+)/gi;
const NEUTRAL_PAINT = /^(?:none|currentcolor|inherit|transparent|black|#0+|rgba?\(0,0,0(?:,1)?)$/i;

/**
 * Whether an SVG draws only in `currentColor`/black (or declares no paint at all, which
 * defaults to black): such an icon is meant to be tinted, not shown as it is.
 */
export function isMonochromeSvg(svg: string): boolean {
  for (const match of svg.matchAll(PAINT)) {
    const value = (match[1] ?? "").replace(/\s+/g, "");
    if (!NEUTRAL_PAINT.test(value)) return false;
  }
  return true;
}

/** The `data:` URI of an SVG document; the document itself is never put in the DOM. */
export function svgDataUri(svg: string): string {
  return `${DATA_URI}${encodeURIComponent(svg)}`;
}

/** WCAG contrast the white glyph keeps against its tile (non-text UI, 1.4.11). */
export const GLYPH_MIN_CONTRAST = 3;

/** Tiles darker than this luminance get the light edge on the dark canvas (MariaDB, Next.js). */
const DARK_TILE_LUMINANCE = 0.05;

const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;

/** The thin inner stroke of a brand tile, and the light edge of a very dark one (tokens). */
const TILE_STROKE = "shadow-[inset_0_0_0_var(--tool-tile-stroke-width)_var(--tool-tile-stroke)]";
const EDGE_STROKE = "shadow-[inset_0_0_0_var(--tool-tile-stroke-width)_var(--tool-tile-edge)]";

/** WCAG contrast of white on `#RRGGBB`. */
export function whiteContrast(hex: string): number {
  return 1.05 / (luminance(hex) + 0.05);
}

export interface BrandTile {
  /** The tile fill: the brand colour, or a darker shade of it when white would be under 3:1. */
  fill: string;
  /** Whether the fill is so dark that the dark canvas needs the light edge stroke. */
  dark: boolean;
}

/**
 * The tile of a brand colour (`metadata.icon_color`): generic, from the colour's luminance only
 * (never from the tool id). Light colours are mixed toward black in 2 % steps until the white
 * glyph reaches {@link GLYPH_MIN_CONTRAST}; anything that is not `#RRGGBB` gives `null` (the
 * caller falls back to the monogram).
 */
export function brandTile(color: string | null | undefined): BrandTile | null {
  if (color === null || color === undefined || !HEX_COLOR.test(color)) return null;
  let fill = color.toUpperCase();
  for (let step = 1; whiteContrast(fill) < GLYPH_MIN_CONTRAST && step <= 50; step += 1) {
    fill = mixHex(color, "black", step / 50).toUpperCase();
  }
  return { fill, dark: luminance(fill) < DARK_TILE_LUMINANCE };
}

/** `list`: Services rows and Dashboard tiles; `grid`: Catalog cards and onboarding; `hero`: page header. */
export type ToolTileVariant = "list" | "grid" | "hero";

/** Edge, glyph and radius (px) of a tile variant, from the `tool-tile` tokens. */
export function toolTileMetrics(variant: ToolTileVariant): {
  size: number;
  glyph: number;
  radius: number;
} {
  return {
    size: toolTile[`${variant}-size`],
    glyph: toolTile[`${variant}-glyph`],
    radius: toolTile[`${variant}-radius`],
  };
}

export type ToolTileKind = "brand" | "image" | "monogram";

export interface ToolTileProps {
  /** Tool id (`mysql`): picks the fallback palette colour and the monogram. */
  tool: string;
  /** The SVG of `tools.icon`, when there is one. */
  svg?: string | undefined;
  /** Or its data URI, when the caller caches URIs (Catalog). */
  uri?: string | undefined;
  /** `metadata.icon_color` (`#RRGGBB`) of the entry: the tile colour of the white glyph. */
  color?: string | null | undefined;
  /** Box size; the box is the same with or without the icon (no layout shift). */
  variant: ToolTileVariant;
  /** Known monochrome-ness of `uri` (the caller saw the SVG); default: from `svg`. */
  monochrome?: boolean | undefined;
}

function monogramText(variant: ToolTileVariant): string {
  if (variant === "list") return "text-callout";
  if (variant === "grid") return "text-headline";
  return "text-title-2";
}

/** The tile: the white logo on the brand colour, a full-colour image, or a monogram. */
export function ToolTile({ tool, svg, uri, color, variant, monochrome }: ToolTileProps) {
  const src = uri ?? (svg === undefined ? undefined : svgDataUri(svg));
  const mono = monochrome ?? (svg === undefined ? false : isMonochromeSvg(svg));
  const brand = brandTile(color);
  const kind: ToolTileKind =
    src === undefined ? "monogram" : !mono ? "image" : brand === null ? "monogram" : "brand";
  const metrics = toolTileMetrics(variant);
  const box: CSSProperties = {
    width: metrics.size,
    height: metrics.size,
    borderRadius: metrics.radius,
  };
  if (kind === "image") {
    return (
      <span
        data-tool-tile="image"
        style={box}
        className="flex shrink-0 items-center justify-center overflow-hidden"
      >
        <img alt="" src={src} style={{ width: metrics.size, height: metrics.size }} />
      </span>
    );
  }
  if (kind === "brand" && brand !== null) {
    // The brand colour reaches CSS only through --tool-brand (no colour literal in the
    // component); forced colours keep it, since the white glyph needs its tile to be seen.
    const style = { ...box, "--tool-brand": brand.fill } as CSSProperties;
    return (
      <span
        aria-hidden="true"
        data-tool-tile="brand"
        data-tool-edge={brand.dark ? "dark" : undefined}
        style={style}
        className={cx(
          "flex shrink-0 items-center justify-center overflow-hidden bg-(--tool-brand) forced-color-adjust-none",
          brand.dark ? EDGE_STROKE : TILE_STROKE,
        )}
      >
        {/* brightness(0) makes every drawn pixel black, invert(1) white: the logo keeps its
            shape and alpha. */}
        <img
          alt=""
          src={src}
          style={{ width: metrics.glyph, height: metrics.glyph }}
          className="brightness-0 invert"
        />
      </span>
    );
  }
  return (
    <span
      aria-hidden="true"
      data-tool-tile="monogram"
      data-tool-color={toolColorIndex(tool) + 1}
      style={box}
      className={cx(
        "flex shrink-0 items-center justify-center overflow-hidden font-semibold text-fg-on-tool",
        toolTileClass(tool),
        monogramText(variant),
      )}
    >
      {toolMonogram(tool)}
    </span>
  );
}
