const UNIT_MS: Record<string, number> = { m: 60_000, h: 3_600_000, d: 86_400_000, w: 604_800_000 };

/** Parse a duration like "30m", "2h", "7d", "1w" into milliseconds. */
export function parseDuration(value: string): number | null {
  const match = value.match(/^(\d+)([mhdw])$/);
  return match ? Number(match[1]) * UNIT_MS[match[2]] : null;
}

function parseRelativeTime(value: string): Date | null {
  const ms = parseDuration(value);
  if (ms === null) return null;
  // A duration reaching before 1970 is a typo, and one past Date's range would be an invalid Date.
  const t = Date.now() - ms;
  return t >= 0 ? new Date(t) : null;
}

function parseAbsoluteTime(value: string): Date | null {
  const day = /^(\d{4})-(\d{2})-(\d{2})/.exec(value);
  if (!day) return null;
  const [y, m, d] = day.slice(1).map(Number);
  // 2026-02-30 or month 13 is an error, not a roll-over into the next month or year.
  const probe = new Date(Date.UTC(y, m - 1, d));
  if (probe.getUTCMonth() !== m - 1 || probe.getUTCDate() !== d) return null;

  // Date-only (YYYY-MM-DD) is local midnight; new Date('YYYY-MM-DD') would be UTC.
  if (value.length === 10) return new Date(y, m - 1, d);

  // A date and time; anything else (`7`, `2025`) is an error, not a guess.
  if (!/^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}/.test(value)) return null;
  const date = new Date(value);
  return isNaN(date.getTime()) ? null : date;
}

export function parseTimeSpec(value: string): Date | null {
  return parseRelativeTime(value) ?? parseAbsoluteTime(value);
}
