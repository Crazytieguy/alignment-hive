// Constants and helpers shared by the analytics beacon and the server, kept free of zod so the
// beacon stays small.

export const NAME = /^[a-z0-9:-]{1,48}$/;
export const VENUE = /^[a-z0-9-]{1,32}$/;

export const VIEWPORTS = ["phone", "tablet", "desktop"] as const;
// Engaged seconds, bucketed on the client: the exact number would add little and identify more.
export const ENGAGED_BUCKETS = [
  "0-10",
  "10-30",
  "30-60",
  "60-180",
  "180-600",
  "600+",
] as const;
export const MAX_EVENTS_PER_BATCH = 20;
export const MAX_BODY_BYTES = 8 * 1024;

/** The engaged-seconds bucket a count falls in. */
export function engagedBucket(seconds: number): (typeof ENGAGED_BUCKETS)[number] {
  if (seconds < 10) return "0-10";
  if (seconds < 30) return "10-30";
  if (seconds < 60) return "30-60";
  if (seconds < 180) return "60-180";
  if (seconds < 600) return "180-600";
  return "600+";
}

/** A `?r=` or `utm_source` value if it's an allowlisted venue tag, else null. */
export function venueFrom(params: URLSearchParams): string | null {
  const raw = (params.get("r") ?? params.get("utm_source") ?? "").toLowerCase();
  return VENUE.test(raw) ? raw : null;
}
