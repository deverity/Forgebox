import {
  isMonochromeSvg,
  svgDataUri,
  ToolTile,
  toolTileMetrics,
  type ToolTileVariant,
} from "../../design/primitives/ToolTile";
import { useForgeQuery } from "../../state";
import { useVisibleOnce } from "./hooks";

// Icons cached by the SHA-256 of their SVG (`CatalogEntryDto.icon_sha256`): the same icon is
// never fetched twice, whatever tool or card asks for it. The data URI and whether the SVG is
// monochrome (drawn white on the brand colour, M2-E1-T24) are kept; the document itself is not.
interface CachedIcon {
  uri: string;
  monochrome: boolean;
}
const BY_SHA = new Map<string, CachedIcon>();

/** Forgets the icon cache (tests). */
export function resetIconCache(): void {
  BY_SHA.clear();
}

export interface CatalogIconProps {
  tool: string;
  /** `icon_sha256` of the entry; without it the tool has no icon and no call is made. */
  sha256: string | null | undefined;
  /** `CatalogEntryDto.icon_color`: the brand colour of the tile (M2-E1-T24). */
  color?: string | null | undefined;
  /** Tile size (the box is reserved before the icon arrives: no layout shift). */
  variant?: ToolTileVariant;
}

/**
 * A tool icon from `tools.icon`, loaded lazily when the card scrolls into view and rendered by
 * the shared {@link ToolTile} ONLY as an `<img>` with a data URI (docs/07 section 7, docs/11):
 * an SVG from a manifest can carry scripts, handlers and external references, none of which
 * run inside an image. The official logo is drawn white on its brand colour; without an icon
 * or a colour a coloured monogram holds the same box.
 */
export function CatalogIcon({ tool, sha256, color, variant = "grid" }: CatalogIconProps) {
  const [ref, visible] = useVisibleOnce<HTMLSpanElement>();
  const cached = sha256 === null || sha256 === undefined ? undefined : BY_SHA.get(sha256);
  const query = useForgeQuery(
    "tools.icon",
    { tool },
    { enabled: visible && cached === undefined && sha256 !== null && sha256 !== undefined },
  );
  let icon = cached;
  if (icon === undefined && query.data !== undefined) {
    icon = { uri: svgDataUri(query.data.svg), monochrome: isMonochromeSvg(query.data.svg) };
    BY_SHA.set(query.data.sha256, icon);
  }
  const { size } = toolTileMetrics(variant);
  const style = { width: size, height: size };
  return (
    <span ref={ref} data-tool-icon={tool} style={style} className="flex shrink-0">
      <ToolTile
        tool={tool}
        uri={icon?.uri}
        monochrome={icon?.monochrome}
        color={color}
        variant={variant}
      />
    </span>
  );
}
