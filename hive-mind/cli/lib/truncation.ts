/**
 * Word-level truncation utilities for adaptive output sizing.
 *
 * Core idea: find a uniform truncation length L such that sum(min(wi, L)) ≈ target.
 * Longer messages get truncated more, short messages shown in full.
 */

const MIN_WORD_LIMIT = 6;

export function countWords(text: string): number {
  if (!text) return 0;
  return text.split(/\s+/).filter((w) => w.length > 0).length;
}

/**
 * Truncate text to a word limit, optionally skipping initial words.
 * Preserves original whitespace.
 */
export function truncateWords(
  text: string,
  skip: number,
  limit: number,
): {
  text: string;
  wordCount: number;
  remaining: number;
  truncated: boolean;
} {
  const wordPattern = /\S+/g;
  const matches: Array<{ word: string; start: number; end: number }> = [];
  let match;
  while ((match = wordPattern.exec(text)) !== null) {
    matches.push({ word: match[0], start: match.index, end: match.index + match[0].length });
  }

  const totalWords = matches.length;

  if (skip >= totalWords) {
    return { text: '', wordCount: 0, remaining: 0, truncated: false };
  }

  const afterSkipCount = totalWords - skip;
  const startIdx = skip;
  const endIdx = Math.min(skip + limit, totalWords);
  const wordsToInclude = endIdx - startIdx;

  if (wordsToInclude === 0) {
    return { text: '', wordCount: 0, remaining: 0, truncated: false };
  }

  const startPos = matches[startIdx].start;
  const endPos = matches[endIdx - 1].end;
  const extracted = text.slice(startPos, endPos);

  const remaining = afterSkipCount - wordsToInclude;
  return {
    text: extracted,
    wordCount: wordsToInclude,
    remaining,
    truncated: remaining > 0,
  };
}

/**
 * Compute a uniform word limit L such that sum(min(wi, L)) ≈ target.
 * Returns null if no truncation needed. Result clamped to MIN_WORD_LIMIT.
 */
export function computeUniformLimit(wordCounts: Array<number>, targetTotal: number): number | null {
  if (wordCounts.length === 0) return null;

  const total = wordCounts.reduce((a, b) => a + b, 0);
  if (total <= targetTotal) return null;

  const sorted = [...wordCounts].sort((a, b) => a - b);
  const n = sorted.length;
  let prefixSum = 0;

  for (let k = 0; k < n; k++) {
    const remaining = n - k;
    const L = (targetTotal - prefixSum) / remaining;
    if (L <= sorted[k]) {
      return Math.max(MIN_WORD_LIMIT, Math.floor(L));
    }
    prefixSum += sorted[k];
  }

  return Math.max(MIN_WORD_LIMIT, Math.floor(targetTotal / n));
}
