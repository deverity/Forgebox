import { motion } from "motion/react";
import { useForgeQuery } from "../../state";
import { useReducedMotion } from "../../design/useMotionPreset";

export interface ToolIconProps {
  tool: string;
  /** Shared-element id: the row icon and the sheet header icon glide into each other. */
  layoutId?: string;
}

const DATA_URI = "data:image/svg+xml;charset=utf-8,";

/**
 * The tool's icon from `tools.icon`, rendered ONLY as an `<img>` with a data URI: the SVG
 * document comes from a manifest and is never injected into the DOM, so its scripts and
 * styles cannot run (docs/07 section 7). Without an icon (not installed, not implemented,
 * still loading) a neutral monogram holds the same 24 px box, so nothing shifts.
 */
export function ToolIcon({ tool, layoutId }: ToolIconProps) {
  const reduced = useReducedMotion();
  const icon = useForgeQuery("tools.icon", { tool });
  const svg = icon.data?.svg;
  const shared = layoutId === undefined || reduced ? {} : { layoutId };
  return (
    <motion.span
      {...shared}
      data-tool-icon={tool}
      className="flex size-6 shrink-0 items-center justify-center overflow-hidden rounded-control"
    >
      {svg === undefined ? (
        <span
          aria-hidden="true"
          className="flex size-6 items-center justify-center rounded-control bg-surface-2 text-caption font-semibold text-fg-secondary uppercase"
        >
          {tool.slice(0, 1)}
        </span>
      ) : (
        <img
          alt=""
          width={24}
          height={24}
          src={`${DATA_URI}${encodeURIComponent(svg)}`}
          className="size-6"
        />
      )}
    </motion.span>
  );
}
