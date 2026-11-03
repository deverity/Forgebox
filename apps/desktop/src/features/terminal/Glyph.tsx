// 16 px stroke glyphs of the Terminal screen (docs/07 §2.4). Decorative: the control that holds
// them carries the accessible name.
const PATHS = {
  close: "M5 5l6 6M11 5l-6 6",
  plus: "M8 3v10M3 8h10",
  up: "M4 10l4-4 4 4",
  down: "M4 6l4 4 4-4",
  cross: "M4 4l8 8M12 4l-8 8",
} as const;

export type GlyphName = keyof typeof PATHS;

export function Glyph({ name }: { name: GlyphName }) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
