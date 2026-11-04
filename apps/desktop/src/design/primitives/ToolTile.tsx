import type { CSSProperties } from "react";
import { cx } from "./shared";

// The icon of a tool on every screen that shows one (M2-E1-T23, docs/07 section 2.4):
// - a full-colour SVG from `tools.icon` is shown as it is;
// - a monochrome SVG (only `currentColor`/black, like the registry's line icons) would render
//   black inside an <img> ("black circles"), so it is drawn white on a coloured tile;
// - without an icon, a coloured rounded-square monogram ("My", "Rd") holds the same box.
// The SVG is ONLY ever an <img> with a data URI: a document from a manifest never enters the
// DOM, so its scripts, handlers and references cannot run (docs/07 section 7, docs/11).
// The tile colour is one of the `tool-*` tokens, picked from the tool id (never the manifest's
// `ui.color`, which has no contrast guarantee).

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

export type ToolTileKind = "image" | "tinted" | "monogram";

export interface ToolTileProps {
  /** Tool id (`mysql`): picks the tile colour and the monogram. */
  tool: string;
  /** The SVG of `tools.icon`, when there is one. */
  svg?: string | undefined;
  /** Or its data URI, when the caller caches URIs (Catalog). */
  uri?: string | undefined;
  /** Edge in px; the box is the same with or without the icon (no layout shift). */
  size: number;
  /** Known monochrome-ness of `uri` (the caller saw the SVG); default: from `svg`. */
  monochrome?: boolean | undefined;
}

function monogramText(size: number): string {
  if (size <= 24) return "text-caption";
  if (size <= 32) return "text-callout";
  if (size <= 48) return "text-headline";
  return "text-title-2";
}

/** The tile: an <img> of the icon, the icon whitened on a coloured tile, or a monogram. */
export function ToolTile({ tool, svg, uri, size, monochrome }: ToolTileProps) {
  const src = uri ?? (svg === undefined ? undefined : svgDataUri(svg));
  const mono = monochrome ?? (svg === undefined ? false : isMonochromeSvg(svg));
  const kind: ToolTileKind = src === undefined ? "monogram" : mono ? "tinted" : "image";
  const box: CSSProperties = { width: size, height: size };
  const radius = size > 32 ? "rounded-card" : "rounded-item";
  if (kind === "image") {
    return (
      <span
        data-tool-tile="image"
        style={box}
        className={cx("flex shrink-0 items-center justify-center overflow-hidden", radius)}
      >
        <img alt="" src={src} style={box} />
      </span>
    );
  }
  const glyph = Math.round(size * 0.66);
  return (
    <span
      aria-hidden="true"
      data-tool-tile={kind}
      data-tool-color={toolColorIndex(tool) + 1}
      style={box}
      className={cx(
        "flex shrink-0 items-center justify-center overflow-hidden font-semibold text-fg-on-tool",
        radius,
        toolTileClass(tool),
        kind === "monogram" && monogramText(size),
      )}
    >
      {kind === "tinted" ? (
        // brightness(0) makes every drawn pixel black, invert(1) white: the glyph keeps its
        // shape and alpha and reads on the tile in both themes.
        <img
          alt=""
          src={src}
          style={{ width: glyph, height: glyph }}
          className="brightness-0 invert"
        />
      ) : (
        toolMonogram(tool)
      )}
    </span>
  );
}
