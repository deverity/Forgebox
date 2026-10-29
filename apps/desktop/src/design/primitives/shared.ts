// Class fragments shared by the control primitives. Tokens only (docs/07 section 11).
export function cx(...parts: readonly (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

/** Visible focus ring with the accent colour (docs/07 section 9). */
export const FOCUS_RING =
  "outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent";

export const DISABLED = "disabled:opacity-50 disabled:cursor-not-allowed";

/**
 * Border + surface used by text-like controls (TextField, Select trigger, Combobox input).
 * The border COLOUR is deliberately not here: callers add exactly one (`FIELD_BORDER` or
 * `border-state-failed`), because two colours on one element are resolved by Tailwind's CSS
 * order, not by className order (the error border used to lose to `border-stroke-strong`).
 */
export const FIELD_BASE =
  "h-8 rounded-control border bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary";

export const FIELD_BORDER = "border-stroke-strong";

/** Width tokens for text-like controls (no free className on primitives). */
export type FieldWidth = "sm" | "md" | "full";
export const FIELD_WIDTH: Record<FieldWidth, string> = {
  sm: "w-40",
  md: "w-64",
  full: "w-full",
};

const COLOR_FAMILIES: readonly (readonly [string, RegExp])[] = [
  ["border-color", /^border-(?:stroke|stroke-strong|accent|transparent|state-[a-z]+)$/],
  ["bg-color", /^bg-(?:canvas|surface|surface-2|accent|transparent|state-[a-z]+)$/],
  ["text-color", /^text-(?:fg-[a-z-]+|accent|state-[a-z]+)$/],
];

/**
 * Returns the colour classes that fight over the same property inside one className (same
 * variant prefix, e.g. `border-stroke-strong` + `border-state-failed`). jsdom does not compute
 * CSS, so tests use this to catch conflicts that only show up in a real browser.
 */
export function conflictingColorClasses(className: string): string[] {
  const seen = new Map<string, string[]>();
  for (const cls of className.split(/\s+/).filter(Boolean)) {
    const at = cls.lastIndexOf(":");
    const variant = cls.slice(0, at + 1);
    const base = cls.slice(at + 1);
    for (const [family, re] of COLOR_FAMILIES) {
      if (re.test(base)) {
        const key = `${variant}${family}`;
        seen.set(key, [...(seen.get(key) ?? []), cls]);
      }
    }
  }
  return [...seen.values()].filter((list) => list.length > 1).flat();
}

/** Value of the data-motion attribute: which motion a control currently uses. */
export function motionKind(reduced: boolean): "fade" | "spring" {
  return reduced ? "fade" : "spring";
}

export const SEGMENT_PILL_TESTID = "segment-pill";

/** Spinner centred over its box (IconButton). */
export const SPINNER =
  "absolute size-4 animate-spin rounded-full border-2 border-current border-t-transparent motion-reduce:animate-none";

/** Spinner in the text flow (Button): 16 px, before the label. */
export const SPINNER_INLINE =
  "size-4 shrink-0 animate-spin rounded-full border-2 border-current border-t-transparent motion-reduce:animate-none";
