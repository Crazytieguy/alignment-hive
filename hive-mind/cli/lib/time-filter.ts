export interface TimeRange {
  after: Date | null;
  before: Date | null;
}

function parseRelativeTime(value: string): Date | null {
  const match = value.match(/^(\d+)([mhdw])$/);
  if (!match) return null;

  const amount = parseInt(match[1], 10);
  const unit = match[2];
  const now = Date.now();

  switch (unit) {
    case "m":
      return new Date(now - amount * 60 * 1000);
    case "h":
      return new Date(now - amount * 60 * 60 * 1000);
    case "d":
      return new Date(now - amount * 24 * 60 * 60 * 1000);
    case "w":
      return new Date(now - amount * 7 * 24 * 60 * 60 * 1000);
    default:
      return null;
  }
}

function parseAbsoluteTime(value: string): Date | null {
  // Try date-only format (YYYY-MM-DD) - parse as local time at midnight
  if (/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    const [y, m, d] = value.split("-").map(Number);
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
export function isInTimeRange(timestamp: string | undefined, range: TimeRange): boolean {
  if (!timestamp) return false;

  const date = new Date(timestamp);
  if (isNaN(date.getTime())) return false;

  if (range.after && date < range.after) return false;
  if (range.before && date > range.before) return false;

  return true;
}
