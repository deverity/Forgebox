import { FOCUS_RING, cx } from "../../design/primitives/shared";

/** Text links: `fg-primary` keeps 4.5:1 on every surface (accent text does not at 12 px). */
export const LINK_CLASS = cx(
  "rounded-control text-fg-primary underline underline-offset-2",
  FOCUS_RING,
);
