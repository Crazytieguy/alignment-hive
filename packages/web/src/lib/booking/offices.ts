// Booking configuration. Hardcoded on purpose: the office schedule changes rarely, and
// day-off / lunch exceptions are handled by Yoav blocking time on his Google Calendar
// (FreeBusy hides any slot that overlaps a busy event). Calendar blocks can only close
// days, though — a dated weekday change that must also open new days uses `override`.

import type { DateTime } from "luxon";

export interface ScheduleOverride {
  /** First local date the override applies, inclusive (zero-padded "YYYY-MM-DD"). */
  from: string;
  /** Last local date the override applies, inclusive (zero-padded "YYYY-MM-DD"). */
  until: string;
  /** Weekdays the office is open from `from` through `until`, replacing `weekdays`. */
  weekdays: number[];
}

export interface OfficeConfig {
  /** Shown to bookers and used as the calendar event location. */
  label: string;
  /** IANA timezone the office hours are expressed in. */
  timezone: string;
  /** Luxon weekday numbers the office is open: 1 = Monday … 7 = Sunday. */
  weekdays: number[];
  /**
   * Temporary schedule change: from `from` through `until` (both inclusive),
   * `override.weekdays` replaces `weekdays`. Delete it, together with its
   * "real … schedule" test in slots.test.ts, once `until` has passed.
   */
  override?: ScheduleOverride;
  /** Office opening time, "HH:mm" wall-clock in `timezone`. */
  start: string;
  /** Office closing time (last slot must end by this), "HH:mm" wall-clock in `timezone`. */
  end: string;
}

/** Whether the office is open on the given day (a Luxon DateTime in the office timezone). */
export function isOfficeOpenOn(office: OfficeConfig, day: DateTime): boolean {
  const isoDate = day.toISODate();
  if (isoDate === null) return false; // invalid DateTime: treat as closed
  const { override } = office;
  const inOverride = override && override.from <= isoDate && isoDate <= override.until;
  const weekdays = inOverride ? override.weekdays : office.weekdays;
  return weekdays.includes(day.weekday);
}

export const OFFICES = {
  mats: {
    label: "MATS office",
    timezone: "America/Los_Angeles",
    weekdays: [2, 4], // Tue, Thu
    // Week of 2026-08-10: Mon + Tue instead of Tue + Thu.
    override: { from: "2026-08-10", until: "2026-08-16", weekdays: [1, 2] },
    start: "10:30",
    end: "18:00",
  },
  "far-labs": {
    label: "Far Labs",
    timezone: "America/Los_Angeles",
    weekdays: [3], // Wed
    // Week of 2026-08-10: skipped entirely.
    override: { from: "2026-08-10", until: "2026-08-16", weekdays: [] },
    start: "10:30",
    end: "18:00",
  },
} satisfies Record<string, OfficeConfig>;

export type OfficeSlug = keyof typeof OFFICES;

/** Meeting lengths the booker may choose, in minutes. */
export const DURATIONS = [60, 90, 120] as const;
export type Duration = (typeof DURATIONS)[number];

/** Earliest a slot may be booked: at least this many hours from now (0 = last-minute allowed). */
export const MIN_NOTICE_HOURS = 0;
/** Latest a slot may be booked: at most this many days from now. */
export const HORIZON_DAYS = 21;

export function isOfficeSlug(value: string): value is OfficeSlug {
  return Object.prototype.hasOwnProperty.call(OFFICES, value);
}

export function isDuration(value: number): value is Duration {
  return (DURATIONS as readonly number[]).includes(value);
}
