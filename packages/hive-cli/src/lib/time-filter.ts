const UNIT_MS: Record<string, number> = { m: 60_000, h: 3_600_000, d: 86_400_000, w: 604_800_000 };

/** Parse a duration like "30m", "2h", "7d", "1w" into milliseconds. */
export function parseDuration(value: string): number | null {
  const match = value.match(/^(\d+)([mhdw])$/);
  return match ? Number(match[1]) * UNIT_MS[match[2]] : null;
}

function parseRelativeTime(value: string): Date | null {
  const ms = parseDuration(value);
  return ms === null ? null : new Date(Date.now() - ms);
}

function parseAbsoluteTime(value: string): Date | null {
  // Date-only (YYYY-MM-DD) is local midnight; new Date('YYYY-MM-DD') would be UTC.
  if (/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    const [y, m, d] = value.split('-').map(Number);
    return new Date(y, m - 1, d);
  }

  const date = new Date(value);
  if (!isNaN(date.getTime())) return date;

  return null;
}

export function parseTimeSpec(value: string): Date | null {
  return parseRelativeTime(value) ?? parseAbsoluteTime(value);
}

/** Returns false if the timestamp is missing or invalid. */
export function isInTimeRange(
  timestamp: string | undefined,
  range: { after: Date | null; before: Date | null },
): boolean {
  if (!timestamp) return false;

  const date = new Date(timestamp);
  if (isNaN(date.getTime())) return false;

  if (range.after && date < range.after) return false;
  if (range.before && date > range.before) return false;

  return true;
}
