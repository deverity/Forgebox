import { motion } from "motion/react";
import { useForgeQuery } from "../../state";
import { ToolTile } from "../../design/primitives/ToolTile";
import { useReducedMotion } from "../../design/useMotionPreset";

export interface ToolIconProps {
  tool: string;
  /** Shared-element id: the row icon and the sheet header icon glide into each other. */
  layoutId?: string;
}

/**
 * The tool's icon from `tools.icon` on the Services and Dashboard screens, drawn by the shared
 * {@link ToolTile}: only ever an `<img>` with a data URI (the SVG from a manifest is never put
 * in the DOM, docs/07 section 7); a monochrome icon is whitened on a coloured tile and without
 * an icon (not installed, not implemented, still loading) a coloured monogram holds the same
 * 24 px box, so nothing shifts (M2-E1-T23: no more black circles).
 */
export function ToolIcon({ tool, layoutId }: ToolIconProps) {
  const reduced = useReducedMotion();
  const icon = useForgeQuery("tools.icon", { tool });
  const shared = layoutId === undefined || reduced ? {} : { layoutId };
  return (
    <motion.span {...shared} data-tool-icon={tool} className="flex size-6 shrink-0">
      <ToolTile tool={tool} svg={icon.data?.svg} size={24} />
    </motion.span>
  );
}
