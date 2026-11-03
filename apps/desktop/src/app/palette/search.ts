// Fuzzy search of the command palette (M2-E1-T14): lowercase, diacritic-insensitive, over the
// pt-BR and en-US labels at once. A query matches when every word of it is a substring of the
// text (best, earlier is better) or, failing that, a subsequence of it (typo-tolerant).

/** Lowercase without accents: `Serviços` -> `servicos`. */
export function fold(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

function subsequence(word: string, text: string): boolean {
  let at = 0;
  for (const ch of word) {
    at = text.indexOf(ch, at);
    if (at === -1) return false;
    at += 1;
  }
  return true;
}

/** 0: no match; higher is better. `text` must already be folded. */
export function score(query: string, text: string): number {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return 1;
  let total = 0;
  for (const word of words) {
    const at = text.indexOf(word);
    if (at !== -1) {
      // Word starts rank above mid-word hits; earlier above later.
      const boundary = at === 0 || /[\s/@._-]/.test(text.charAt(at - 1));
      total += (boundary ? 100 : 60) - Math.min(at, 40) / 2;
    } else if (word.length > 3 && subsequence(word, text)) {
      total += 10;
    } else {
      return 0;
    }
  }
  return total;
}
