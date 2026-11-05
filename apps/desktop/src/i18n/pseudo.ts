/** QA-only pseudo-locale: accents, ~+40 % length, brackets to expose truncation and hard-coded text. */

const MAP: Record<string, string> = {
  a: "á",
  b: "ƀ",
  c: "ç",
  d: "ð",
  e: "é",
  f: "ƒ",
  g: "ğ",
  h: "ĥ",
  i: "í",
  j: "ĵ",
  k: "ķ",
  l: "ļ",
  m: "ɱ",
  n: "ñ",
  o: "ó",
  p: "þ",
  q: "ǫ",
  r: "ŕ",
  s: "š",
  t: "ţ",
  u: "ú",
  v: "ṽ",
  w: "ŵ",
  x: "ẋ",
  y: "ý",
  z: "ž",
  A: "Á",
  B: "Ɓ",
  C: "Ç",
  D: "Ð",
  E: "É",
  F: "Ƒ",
  G: "Ğ",
  H: "Ĥ",
  I: "Í",
  J: "Ĵ",
  K: "Ķ",
  L: "Ļ",
  M: "Ṁ",
  N: "Ñ",
  O: "Ó",
  P: "Þ",
  Q: "Ǫ",
  R: "Ŕ",
  S: "Š",
  T: "Ţ",
  U: "Ú",
  V: "Ṽ",
  W: "Ŵ",
  X: "Ẋ",
  Y: "Ý",
  Z: "Ž",
};

/** Matches i18next placeholders and plural/nesting syntax that must stay untouched. */
const PROTECTED = /(\{\{[^}]*\}\}|\{[^}]*\}|\$t\([^)]*\))/g;

function accent(text: string): string {
  let out = "";
  for (const ch of text) out += MAP[ch] ?? ch;
  return out;
}

/** Pseudo-localizes one string: placeholders are preserved, padding brings it to >= 1.4x. */
export function pseudoize(text: string): string {
  const body = text
    .split(PROTECTED)
    .map((part, i) => (i % 2 === 1 ? part : accent(part)))
    .join("");
  const extra = Math.ceil(text.length * 0.4);
  return `[${body}${"~".repeat(extra)}]`;
}

/** Applies {@link pseudoize} to every string leaf of a namespace bundle. */
export function pseudoizeBundle<T>(bundle: T): T {
  if (typeof bundle === "string") return pseudoize(bundle) as T;
  if (bundle !== null && typeof bundle === "object") {
    return Object.fromEntries(Object.entries(bundle).map(([k, v]) => [k, pseudoizeBundle(v)])) as T;
  }
  return bundle;
}
