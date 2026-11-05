/** The primitives of docs/07 section 6 (identifiers, so JSX never carries a bare literal). */
export const P = {
  Button: "Button",
  IconButton: "IconButton",
  Toggle: "Toggle",
  Select: "Select",
  Combobox: "Combobox",
  SegmentedControl: "SegmentedControl",
  Slider: "Slider",
  TextField: "TextField",
  PathField: "PathField",
  PortField: "PortField",
  Badge: "Badge",
  StatusDot: "StatusDot",
  Card: "Card",
  ListRow: "ListRow",
  Sheet: "Sheet",
  Popover: "Popover",
  Menu: "Menu",
  Toast: "Toast",
  Progress: "Progress",
  Sparkline: "Sparkline",
  EmptyState: "EmptyState",
  Skeleton: "Skeleton",
  CommandPalette: "CommandPalette",
  Kbd: "Kbd",
  Tooltip: "Tooltip",
  ToolTile: "ToolTile",
} as const;

/** Attribute that tells which docs/07 section 6 primitive(s) a gallery block shows. */
export interface GalleryMarker {
  "data-gallery": string;
}

/**
 * `{...shows(P.Button)}`: marks a gallery block with the primitive(s) it demonstrates. A test
 * (`Gallery.test.tsx`) compares the marks with the list of docs/07 section 6, so a primitive
 * added to the doc without a gallery block fails the build.
 */
export function shows(...primitives: (typeof P)[keyof typeof P][]): GalleryMarker {
  return { "data-gallery": primitives.join(" ") };
}
