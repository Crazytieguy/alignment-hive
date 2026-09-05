import { describe, expect, test } from 'bun:test';
import { computeUniformLimit, countWords, truncateWords } from '../lib/truncation';

describe('countWords', () => {
  test.each([
    ['', 0],
    ['   \n\t  ', 0],
    ['hello', 1],
    ['hello world foo bar', 4],
    ['hello   world\n\nfoo\tbar', 4],
    ['  hello world  ', 2],
  ])('%j -> %i', (text, n) => {
    expect(countWords(text)).toBe(n);
  });
});

describe('truncateWords', () => {
  test('empty text or skipping every word returns empty', () => {
    expect(truncateWords('', 0, 10)).toEqual({ text: '', remaining: 0 });
    expect(truncateWords('hello world', 2, 10)).toEqual({ text: '', remaining: 0 });
  });

  test('skip 0, text fits in limit', () => {
    expect(truncateWords('hello world', 0, 10)).toEqual({ text: 'hello world', remaining: 0 });
  });

  test('skip 0, text exceeds limit', () => {
    expect(truncateWords('one two three four five', 0, 3)).toEqual({ text: 'one two three', remaining: 2 });
  });

  test('skip some words, remaining fits', () => {
    expect(truncateWords('one two three four five', 2, 10)).toEqual({ text: 'three four five', remaining: 0 });
  });

  test('skip some words, remaining exceeds limit', () => {
    expect(truncateWords('one two three four five', 1, 2)).toEqual({ text: 'two three', remaining: 2 });
  });

  test('preserves the original whitespace between words', () => {
    expect(truncateWords('hello   world\n\nfoo  bar', 0, 2)).toEqual({ text: 'hello   world', remaining: 2 });
  });

  test('skip starts from the skipped word position in the original text', () => {
    expect(truncateWords('one  two   three    four', 1, 2)).toEqual({ text: 'two   three', remaining: 1 });
  });
});

describe('computeUniformLimit', () => {
  test('empty input returns null', () => {
    expect(computeUniformLimit([], 100)).toBeNull();
  });

  test('total fits in target returns null', () => {
    expect(computeUniformLimit([10, 20, 30], 100)).toBeNull();
  });

  test('total exactly equals target returns null', () => {
    expect(computeUniformLimit([10, 20, 30], 60)).toBeNull();
  });

  test('uniform distribution when all equal', () => {
    expect(computeUniformLimit([100, 100, 100], 150)).toBe(50);
  });

  test('short messages shown in full, long truncated', () => {
    // [20, 50, 100], target 100: 20 fits whole, (100 - 20) / 2 = 40 for the rest -> 20 + 40 + 40 = 100
    expect(computeUniformLimit([20, 50, 100], 100)).toBe(40);
  });

  test('very small target enforces minimum of 6', () => {
    expect(computeUniformLimit([100, 200], 1)).toBe(6);
  });

  test('single message over target', () => {
    expect(computeUniformLimit([100], 50)).toBe(50);
  });

  test('many small messages with computed limit below minimum', () => {
    expect(computeUniformLimit(new Array(10).fill(10), 50)).toBe(6);
  });

  test('mixed sizes with some fitting', () => {
    // [5, 10, 15, 100], target 80: 5 + 10 + 15 fit whole, 80 - 30 = 50 for the last
    expect(computeUniformLimit([5, 10, 15, 100], 80)).toBe(50);
  });

  test('order of input does not matter', () => {
    const a = computeUniformLimit([100, 50, 20], 100);
    const b = computeUniformLimit([20, 50, 100], 100);
    const c = computeUniformLimit([50, 100, 20], 100);
    expect(a).toBe(b);
    expect(b).toBe(c);
  });
});
