import { motion } from "motion/react";
import { useForgeQuery } from "../../state";
import { ToolTile, toolTileMetrics } from "../../design/primitives/ToolTile";
import { useReducedMotion } from "../../design/useMotionPreset";

export interface ToolIconProps {
  tool: string;
  /** Shared-element id: the row icon and the sheet header icon glide into each other. */
  layoutId?: string;
}

/**
 * The tool's icon from `tools.icon` on the Services and Dashboard screens, drawn by the shared
 * {@link ToolTile}: only ever an `<img>` with a data URI (the SVG from a manifest is never put
 * in the DOM, docs/07 section 7). The official logo is drawn white on its brand colour
 * (`IconDto.icon_color`, M2-E1-T24); without an icon or colour (not installed, not implemented,
 * still loading, third-party registry) a coloured monogram holds the same `list` box, so nothing
 * shifts.
 */
export function ToolIcon({ tool, layoutId }: ToolIconProps) {
  const reduced = useReducedMotion();
  const icon = useForgeQuery("tools.icon", { tool });
  const shared = layoutId === undefined || reduced ? {} : { layoutId };
  const { size } = toolTileMetrics("list");
  return (
    <motion.span
      {...shared}
      data-tool-icon={tool}
      style={{ width: size, height: size }}
      className="flex shrink-0"
    >
      <ToolTile tool={tool} svg={icon.data?.svg} color={icon.data?.icon_color} variant="list" />
    </motion.span>
  );
}
