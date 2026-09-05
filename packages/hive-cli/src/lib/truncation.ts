const MIN_WORD_LIMIT = 6;

/** Character spans of the whitespace-separated words in text. */
export function splitIntoWords(text: string): Array<{ start: number; end: number }> {
  return [...text.matchAll(/\S+/g)].map((m) => ({ start: m.index, end: m.index + m[0].length }));
}

export function countWords(text: string): number {
  return splitIntoWords(text).length;
}

export function countLines(text: string): number {
  return text ? text.split('\n').length : 0;
}

/** Words [skip, skip + limit) of text, with the original whitespace between them, and how many follow. */
export function truncateWords(text: string, skip: number, limit: number): { text: string; remaining: number } {
  const words = splitIntoWords(text);
  const end = Math.min(skip + limit, words.length);
  if (end <= skip) return { text: '', remaining: 0 };
  return { text: text.slice(words[skip].start, words[end - 1].end), remaining: words.length - end };
}

/**
 * The per-field word limit that brings the total under targetTotal, letting short fields
 * through whole; null when everything already fits.
 */
export function computeUniformLimit(wordCounts: Array<number>, targetTotal: number): number | null {
  if (wordCounts.length === 0) return null;

  const total = wordCounts.reduce((a, b) => a + b, 0);
  if (total <= targetTotal) return null;

  const sorted = [...wordCounts].sort((a, b) => a - b);
  const n = sorted.length;
  let prefixSum = 0;

  for (let k = 0; k < n; k++) {
    const L = (targetTotal - prefixSum) / (n - k);
    if (L <= sorted[k]) return Math.max(MIN_WORD_LIMIT, Math.floor(L));
    prefixSum += sorted[k];
  }

  throw new Error('unreachable: total > targetTotal guarantees some L <= sorted[k]');
}
