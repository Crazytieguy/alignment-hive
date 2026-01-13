import { describe, expect, test } from "bun:test";
import { isInTimeRange, parseTimeSpec } from "../lib/time-filter";

describe("parseTimeSpec", () => {
  describe("relative times", () => {
    test("parses minutes", () => {
      const result = parseTimeSpec("30m");
      expect(result).not.toBeNull();
      const diff = Date.now() - result!.getTime();
      // Should be ~30 minutes ago (with some tolerance for test execution)
      expect(diff).toBeGreaterThan(29 * 60 * 1000);
      expect(diff).toBeLessThan(31 * 60 * 1000);
    });

    test("parses hours", () => {
      const result = parseTimeSpec("2h");
      expect(result).not.toBeNull();
      const diff = Date.now() - result!.getTime();
      expect(diff).toBeGreaterThan(119 * 60 * 1000);
      expect(diff).toBeLessThan(121 * 60 * 1000);
    });

    test("parses days", () => {
      const result = parseTimeSpec("7d");
      expect(result).not.toBeNull();
      const diff = Date.now() - result!.getTime();
      const sevenDaysMs = 7 * 24 * 60 * 60 * 1000;
      expect(diff).toBeGreaterThan(sevenDaysMs - 60 * 1000);
      expect(diff).toBeLessThan(sevenDaysMs + 60 * 1000);
    });

    test("parses weeks", () => {
      const result = parseTimeSpec("1w");
      expect(result).not.toBeNull();
      const diff = Date.now() - result!.getTime();
      const oneWeekMs = 7 * 24 * 60 * 60 * 1000;
      expect(diff).toBeGreaterThan(oneWeekMs - 60 * 1000);
      expect(diff).toBeLessThan(oneWeekMs + 60 * 1000);
    });

    test("returns null for invalid relative time", () => {
      expect(parseTimeSpec("30x")).toBeNull();
      expect(parseTimeSpec("abc")).toBeNull();
      expect(parseTimeSpec("m30")).toBeNull();
    });
  });

  describe("absolute times", () => {
    test("parses date-only format (YYYY-MM-DD)", () => {
      const result = parseTimeSpec("2025-01-15");
      expect(result).not.toBeNull();
      expect(result!.getFullYear()).toBe(2025);
      expect(result!.getMonth()).toBe(0); // January
      expect(result!.getDate()).toBe(15);
    });

    test("parses ISO 8601 with time", () => {
      const result = parseTimeSpec("2025-01-15T14:30:00Z");
      expect(result).not.toBeNull();
      expect(result!.toISOString()).toBe("2025-01-15T14:30:00.000Z");
    });

    test("parses date with time without timezone", () => {
      const result = parseTimeSpec("2025-01-15T14:30");
      expect(result).not.toBeNull();
      expect(result!.getFullYear()).toBe(2025);
      expect(result!.getHours()).toBe(14);
      expect(result!.getMinutes()).toBe(30);
    });

    test("returns null for invalid date", () => {
      expect(parseTimeSpec("not-a-date")).toBeNull();
      expect(parseTimeSpec("yesterday")).toBeNull();
    });
  });
});

describe("isInTimeRange", () => {
  test("returns true when timestamp is within range", () => {
    const result = isInTimeRange("2025-01-15T12:00:00Z", {
      after: new Date("2025-01-01T00:00:00Z"),
      before: new Date("2025-12-31T23:59:59Z"),
    });
    expect(result).toBe(true);
  });

  test("returns false when timestamp is before 'after'", () => {
    const result = isInTimeRange("2024-01-01T00:00:00Z", {
      after: new Date("2025-01-01T00:00:00Z"),
      before: null,
    });
    expect(result).toBe(false);
  });

  test("returns false when timestamp is after 'before'", () => {
    const result = isInTimeRange("2026-01-01T00:00:00Z", {
      after: null,
      before: new Date("2025-12-31T23:59:59Z"),
    });
    expect(result).toBe(false);
  });

  test("returns true with only 'after' constraint met", () => {
    const result = isInTimeRange("2025-06-01T00:00:00Z", {
      after: new Date("2025-01-01T00:00:00Z"),
      before: null,
    });
    expect(result).toBe(true);
  });

  test("returns true with only 'before' constraint met", () => {
    const result = isInTimeRange("2025-01-01T00:00:00Z", {
      after: null,
      before: new Date("2025-12-31T23:59:59Z"),
    });
    expect(result).toBe(true);
  });

  test("returns true when no constraints", () => {
    const result = isInTimeRange("2025-01-15T12:00:00Z", {
      after: null,
      before: null,
    });
    expect(result).toBe(true);
  });

  test("returns false for undefined timestamp", () => {
    const result = isInTimeRange(undefined, {
      after: new Date("2025-01-01T00:00:00Z"),
      before: null,
    });
    expect(result).toBe(false);
  });

  test("returns false for invalid timestamp", () => {
    const result = isInTimeRange("not-a-date", {
      after: new Date("2025-01-01T00:00:00Z"),
      before: null,
    });
    expect(result).toBe(false);
  });
});
