import { describe, expect, test } from "bun:test";
import { computeUniformLimit, countWords, truncateWords } from "../lib/truncation";

describe("countWords", () => {
  test("empty string returns 0", () => {
    expect(countWords("")).toBe(0);
  });

  test("whitespace only returns 0", () => {
    expect(countWords("   \n\t  ")).toBe(0);
  });

  test("single word", () => {
    expect(countWords("hello")).toBe(1);
  });

  test("multiple words separated by spaces", () => {
    expect(countWords("hello world foo bar")).toBe(4);
  });

  test("multiple words with varied whitespace", () => {
    expect(countWords("hello   world\n\nfoo\tbar")).toBe(4);
  });

  test("leading and trailing whitespace ignored", () => {
    expect(countWords("  hello world  ")).toBe(2);
  });
});

describe("truncateWords", () => {
  test("empty text returns empty", () => {
    expect(truncateWords("", 0, 10)).toEqual({
      text: "",
      wordCount: 0,
      remaining: 0,
      truncated: false,
    });
  });

  test("skip 0, text fits in limit", () => {
    expect(truncateWords("hello world", 0, 10)).toEqual({
      text: "hello world",
      wordCount: 2,
      remaining: 0,
      truncated: false,
    });
  });

  test("skip 0, text exceeds limit", () => {
    expect(truncateWords("one two three four five", 0, 3)).toEqual({
      text: "one two three",
      wordCount: 3,
      remaining: 2,
      truncated: true,
    });
  });

  test("skip some words, remaining fits", () => {
    expect(truncateWords("one two three four five", 2, 10)).toEqual({
      text: "three four five",
      wordCount: 3,
      remaining: 0,
      truncated: false,
    });
  });

  test("skip some words, remaining exceeds limit", () => {
    expect(truncateWords("one two three four five", 1, 2)).toEqual({
      text: "two three",
      wordCount: 2,
      remaining: 2,
      truncated: true,
    });
  });

  test("skip all words returns empty", () => {
    expect(truncateWords("hello world", 5, 10)).toEqual({
      text: "",
      wordCount: 0,
      remaining: 0,
      truncated: false,
    });
  });

  test("skip exactly word count returns empty", () => {
    expect(truncateWords("hello world", 2, 10)).toEqual({
      text: "",
      wordCount: 0,
      remaining: 0,
      truncated: false,
    });
  });

  test("handles varied whitespace", () => {
    // Preserves original whitespace between words
    expect(truncateWords("hello   world\n\nfoo  bar", 0, 2)).toEqual({
      text: "hello   world",
      wordCount: 2,
      remaining: 2,
      truncated: true,
    });
  });

  test("skip preserves whitespace from skip point", () => {
    // When skipping, start from the skip word's position in original text
    expect(truncateWords("one  two   three    four", 1, 2)).toEqual({
      text: "two   three",
      wordCount: 2,
      remaining: 1,
      truncated: true,
    });
  });
});

describe("computeUniformLimit", () => {
  test("empty input returns null", () => {
    expect(computeUniformLimit([], 100)).toBeNull();
  });

  test("total fits in target returns null", () => {
    expect(computeUniformLimit([10, 20, 30], 100)).toBeNull();
  });

  test("total exactly equals target returns null", () => {
    expect(computeUniformLimit([10, 20, 30], 60)).toBeNull();
  });

  test("uniform distribution when all equal", () => {
    // 3 messages of 100 words each, target 150
    // Limit should be 50 (150/3)
    expect(computeUniformLimit([100, 100, 100], 150)).toBe(50);
  });

  test("short messages shown in full, long truncated", () => {
    // Messages: [20, 50, 100] = 170 total, target 100
    // Short messages (20, 50) should be shown in full if possible
    // Algorithm: L = (100 - 20) / 2 = 40 for messages >= 50
    // Check: 20 + 40 + 40 = 100
    expect(computeUniformLimit([20, 50, 100], 100)).toBe(40);
  });

  test("very small target enforces minimum of 6", () => {
    // Even with tiny target, should get at least 6 words (MIN_WORD_LIMIT)
    expect(computeUniformLimit([100, 200], 1)).toBe(6);
  });

  test("single message over target", () => {
    expect(computeUniformLimit([100], 50)).toBe(50);
  });

  test("many small messages with computed limit below minimum", () => {
    // 10 messages of 10 words each = 100 total, target 50
    // Computed limit would be 5 (50/10), but minimum is 6
    expect(computeUniformLimit(new Array(10).fill(10), 50)).toBe(6);
  });

  test("mixed sizes with some fitting", () => {
    // Messages: [5, 10, 15, 100] = 130 total, target 80
    // Short messages (5, 10, 15) might fit, 100 truncated
    // If we include all short: 5+10+15 = 30, remaining = 50 for 1 message = 50
    // Check: 5 + 10 + 15 + 50 = 80
    expect(computeUniformLimit([5, 10, 15, 100], 80)).toBe(50);
  });

  test("order of input does not matter", () => {
    const a = computeUniformLimit([100, 50, 20], 100);
    const b = computeUniformLimit([20, 50, 100], 100);
    const c = computeUniformLimit([50, 100, 20], 100);
    expect(a).toBe(b);
    expect(b).toBe(c);
  });
});
