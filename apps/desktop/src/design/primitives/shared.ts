// Class fragments shared by the control primitives. Tokens only (docs/07 section 11).
export function cx(...parts: readonly (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

/** Visible focus ring with the accent colour (docs/07 section 9). */
export const FOCUS_RING =
  "outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent";

export const DISABLED = "disabled:opacity-50 disabled:cursor-not-allowed";

/** Border + surface used by text-like controls (TextField, Select trigger, Combobox input). */
export const FIELD_BASE =
  "h-8 rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary";

/** Value of the data-motion attribute: which motion a control currently uses. */
export function motionKind(reduced: boolean): "fade" | "spring" {
  return reduced ? "fade" : "spring";
}

export const SEGMENT_PILL_TESTID = "segment-pill";

export const SPINNER =
  "absolute size-4 animate-spin rounded-full border-2 border-current border-t-transparent motion-reduce:animate-none";
