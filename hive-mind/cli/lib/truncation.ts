/**
 * Word-level truncation utilities for adaptive output sizing.
 *
 * The core idea: find a uniform truncation length L (in words) such that
 * applying min(message_length, L) to all messages hits a target total.
 * This ensures longer messages get truncated more, while short messages
 * are shown in full.
 */

/** Minimum words per message to ensure useful content */
const MIN_WORD_LIMIT = 6;

/**
 * Count words in text (whitespace-separated tokens).
 */
export function countWords(text: string): number {
  if (!text) return 0;
  return text.split(/\s+/).filter((w) => w.length > 0).length;
}

/**
 * Truncate text to a word limit, optionally skipping initial words.
 * Preserves original whitespace (doesn't collapse spaces or newlines).
 *
 * @param text - The text to truncate
 * @param skip - Number of words to skip from the start
 * @param limit - Maximum words to include after skipping
 * @returns Object with truncated text, word count, remaining words, and truncation flag
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
  // Find word boundaries while preserving original whitespace
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

  // Extract substring preserving original whitespace
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
 * Compute a uniform word limit that achieves a target total word count.
 *
 * Given N messages with word counts [w1, w2, ..., wN] and target T:
 * - If sum(all) <= T: return null (no truncation needed)
 * - Otherwise: find L such that sum(min(wi, L)) ≈ T
 *
 * The algorithm finds the optimal L by sorting messages and solving
 * algebraically. Messages shorter than L are shown in full; longer
 * messages are truncated to exactly L words.
 *
 * The result is clamped to MIN_WORD_LIMIT (6) to ensure useful content.
 *
 * @param wordCounts - Array of word counts for each message
 * @param targetTotal - Target total word count
 * @returns Uniform limit L, or null if no truncation needed
 */
export function computeUniformLimit(wordCounts: Array<number>, targetTotal: number): number | null {
  if (wordCounts.length === 0) return null;

  const total = wordCounts.reduce((a, b) => a + b, 0);
  if (total <= targetTotal) return null;

  // Sort ascending to find the optimal breakpoint
  const sorted = [...wordCounts].sort((a, b) => a - b);
  const n = sorted.length;
  let prefixSum = 0;

  for (let k = 0; k < n; k++) {
    const remaining = n - k;
    const L = (targetTotal - prefixSum) / remaining;

    // If L fits before the next message size, this is our limit
    if (L <= sorted[k]) {
      return Math.max(MIN_WORD_LIMIT, Math.floor(L));
    }

    prefixSum += sorted[k];
  }

  // Fallback: distribute evenly (shouldn't normally reach here)
  return Math.max(MIN_WORD_LIMIT, Math.floor(targetTotal / n));
}
