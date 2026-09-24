import { describe, expect, test } from 'bun:test';
import { parseDuration, parseTimeSpec } from '../lib/time-filter';

describe('parseDuration', () => {
  test.each([
    ['30m', 30 * 60 * 1000],
    ['2h', 2 * 60 * 60 * 1000],
    ['7d', 7 * 24 * 60 * 60 * 1000],
    ['1w', 7 * 24 * 60 * 60 * 1000],
  ])('%s is %i ms', (spec, ms) => {
    expect(parseDuration(spec)).toBe(ms);
  });

  test('returns null for anything else', () => {
    for (const spec of ['30x', 'abc', 'm30', '', '1.5h']) expect(parseDuration(spec)).toBeNull();
  });
});

describe('parseTimeSpec', () => {
  test('relative specs are that long before now', () => {
    const diff = Date.now() - parseTimeSpec('2h')!.getTime();
    expect(Math.abs(diff - 2 * 60 * 60 * 1000)).toBeLessThan(60 * 1000);
  });

  test('date-only specs are local midnight, not UTC', () => {
    const d = parseTimeSpec('2025-01-15')!;
    expect([d.getFullYear(), d.getMonth(), d.getDate(), d.getHours()]).toEqual([2025, 0, 15, 0]);
  });

  test('ISO date-times parse with and without an offset', () => {
    expect(parseTimeSpec('2025-01-15T14:30:00Z')!.toISOString()).toBe('2025-01-15T14:30:00.000Z');
    for (const spec of ['2025-01-15T14:30', '2025-01-15 14:30']) {
      const local = parseTimeSpec(spec)!;
      expect([local.getHours(), local.getMinutes()]).toEqual([14, 30]);
    }
  });

  test('returns null for unparseable specs', () => {
    for (const spec of ['30x', 'abc', 'm30', 'not-a-date', 'yesterday', '7', '2025', 'Sep 12'])
      expect(parseTimeSpec(spec)).toBeNull();
  });

  test('impossible dates and durations reaching before 1970 are errors, not roll-overs', () => {
    for (const spec of ['2026-02-30', '2026-13-45', '2026-00-10', '2026-02-30T10:00', '2026-09-12T25:00', '1000000000d'])
      expect(parseTimeSpec(spec)).toBeNull();
    expect(parseTimeSpec('2024-02-29')).not.toBeNull();
  });
});
