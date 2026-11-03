import { useForgeQuery } from "../../state";
import { useVisibleOnce } from "./hooks";

const DATA_URI = "data:image/svg+xml;charset=utf-8,";

// Icons cached by the SHA-256 of their SVG (`CatalogEntryDto.icon_sha256`): the same icon is
// never fetched twice, whatever tool or card asks for it.
const BY_SHA = new Map<string, string>();

/** Forgets the icon cache (tests). */
export function resetIconCache(): void {
  BY_SHA.clear();
}

/** The `data:` URI of an SVG document; the document itself is never put in the DOM. */
export function svgDataUri(svg: string): string {
  return `${DATA_URI}${encodeURIComponent(svg)}`;
}

export interface CatalogIconProps {
  tool: string;
  /** `icon_sha256` of the entry; without it the tool has no icon and no call is made. */
  sha256: string | null | undefined;
  /** Edge in px (the box is reserved before the icon arrives: no layout shift). */
  size?: number;
}

/**
 * A tool icon from `tools.icon`, loaded lazily when the card scrolls into view and rendered
 * ONLY as an `<img>` with a data URI (docs/07 section 7, docs/11): an SVG from a manifest
 * can carry scripts, handlers and external references, none of which run inside an image.
 * Without an icon a neutral monogram holds the same box.
 */
export function CatalogIcon({ tool, sha256, size = 32 }: CatalogIconProps) {
  const [ref, visible] = useVisibleOnce<HTMLSpanElement>();
  const cached = sha256 === null || sha256 === undefined ? undefined : BY_SHA.get(sha256);
  const query = useForgeQuery(
    "tools.icon",
    { tool },
    { enabled: visible && cached === undefined && sha256 !== null && sha256 !== undefined },
  );
  let uri = cached;
  if (uri === undefined && query.data !== undefined) {
    uri = svgDataUri(query.data.svg);
    BY_SHA.set(query.data.sha256, uri);
  }
  const style = { width: size, height: size };
  return (
    <span
      ref={ref}
      data-tool-icon={tool}
      style={style}
      className="flex shrink-0 items-center justify-center overflow-hidden rounded-control"
    >
      {uri === undefined ? (
        <span
          aria-hidden="true"
          style={style}
          className="flex items-center justify-center rounded-control bg-surface-2 text-callout font-semibold text-fg-secondary uppercase"
        >
          {tool.slice(0, 1)}
        </span>
      ) : (
        <img alt="" src={uri} style={style} />
      )}
    </span>
  );
}
