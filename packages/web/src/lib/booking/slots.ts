import { DateTime, Interval } from "luxon";
import { HORIZON_DAYS, MIN_NOTICE_HOURS, type OfficeConfig, isOfficeOpenOn } from "./offices";

const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

/** Bookers can start a meeting on any 30-minute boundary within office hours. */
export const SLOT_STEP_MINUTES = 30;

/** A busy interval as returned by Google FreeBusy (RFC3339 strings). */
export interface BusyInterval {
  start: string;
  end: string;
}

/** An open meeting slot, as UTC epoch milliseconds. */
export interface Slot {
  startUtc: number;
  endUtc: number;
}

function parseHm(hm: string): { hour: number; minute: number } {
  const [hour, minute] = hm.split(":").map(Number);
  return { hour, minute };
}

/**
 * The [from, to] window (UTC ms) availability is computed over, derived from `now`:
 * from `now + MIN_NOTICE_HOURS` to `now + HORIZON_DAYS`. The booking server route uses
 * this to bound its Google FreeBusy query.
 */
export function availabilityWindowUtc(nowUtc: number): { fromUtc: number; toUtc: number } {
  return {
    fromUtc: nowUtc + MIN_NOTICE_HOURS * HOUR_MS,
    toUtc: nowUtc + HORIZON_DAYS * DAY_MS,
  };
}

/**
 * Generate the open slots for an office + meeting duration.
 *
 * Slots are built from wall-clock time in the office's IANA timezone and converted to UTC
 * only at the end, so they stay DST-correct (a 10:30 office meeting is 10:30 local whether
 * or not DST is in effect). Days are iterated with calendar arithmetic (`plus({ days })`),
 * never by adding a fixed number of milliseconds, so DST transitions don't drift the time.
 *
 * Start times are offered on every SLOT_STEP_MINUTES (30-min) boundary within office hours,
 * for a meeting of `durationMin` that fully fits before closing. A slot is excluded if it
 * overlaps any `busy` interval (this is how lunch, days off, and existing meetings disappear —
 * Yoav blocks them on his calendar) or falls outside the [now + MIN_NOTICE_HOURS,
 * now + HORIZON_DAYS] window. This one function backs both the on-page time picker and the
 * server-side revalidation at booking time.
 */
export function generateSlots(
  office: OfficeConfig,
  durationMin: number,
  busy: BusyInterval[],
  nowUtc: number,
): Slot[] {
  const zone = office.timezone;
  const { hour: startH, minute: startM } = parseHm(office.start);
  const { hour: endH, minute: endM } = parseHm(office.end);
  const { fromUtc, toUtc } = availabilityWindowUtc(nowUtc);

  const busyIntervals = busy.map((b) =>
    Interval.fromDateTimes(DateTime.fromISO(b.start), DateTime.fromISO(b.end)),
  );

  const slots: Slot[] = [];

  let day = DateTime.fromMillis(fromUtc, { zone }).startOf("day");
  const lastDay = DateTime.fromMillis(toUtc, { zone }).startOf("day");

  while (day <= lastDay) {
    if (isOfficeOpenOn(office, day)) {
      const dayEnd = day.set({ hour: endH, minute: endM, second: 0, millisecond: 0 });
      let slotStart = day.set({ hour: startH, minute: startM, second: 0, millisecond: 0 });

      while (true) {
        const slotEnd = slotStart.plus({ minutes: durationMin });
        if (slotEnd > dayEnd) break;

        const startUtc = slotStart.toUTC().toMillis();
        const endUtc = slotEnd.toUTC().toMillis();

        // Require the whole slot inside the window: a slot whose tail extends past `toUtc`
        // is past the FreeBusy query bound, so a busy event in that tail wouldn't hide it.
        if (startUtc >= fromUtc && endUtc <= toUtc) {
          const slotInterval = Interval.fromDateTimes(slotStart, slotEnd);
          if (!busyIntervals.some((b) => b.overlaps(slotInterval))) {
            slots.push({ startUtc, endUtc });
          }
        }
        slotStart = slotStart.plus({ minutes: SLOT_STEP_MINUTES }); // 30-min start increments
      }
    }
    day = day.plus({ days: 1 });
  }

  return slots;
}

/** The [open, close] window (UTC ms) for each office day inside the booking window. */
export function officeOpenWindows(
  office: OfficeConfig,
  nowUtc: number,
): { startUtc: number; endUtc: number }[] {
  const zone = office.timezone;
  const { hour: startH, minute: startM } = parseHm(office.start);
  const { hour: endH, minute: endM } = parseHm(office.end);
  const { fromUtc, toUtc } = availabilityWindowUtc(nowUtc);

  const windows: { startUtc: number; endUtc: number }[] = [];
  let day = DateTime.fromMillis(fromUtc, { zone }).startOf("day");
  const lastDay = DateTime.fromMillis(toUtc, { zone }).startOf("day");
  while (day <= lastDay) {
    if (isOfficeOpenOn(office, day)) {
      const open = day.set({ hour: startH, minute: startM }).toUTC().toMillis();
      const close = day.set({ hour: endH, minute: endM }).toUTC().toMillis();
      const startUtc = Math.max(open, fromUtc);
      const endUtc = Math.min(close, toUtc);
      if (startUtc < endUtc) windows.push({ startUtc, endUtc });
    }
    day = day.plus({ days: 1 });
  }
  return windows;
}

/**
 * Round busy intervals outward to the 30-min slot grid before they leave the server. Slots start
 * on 30-min boundaries, so any busy time touching a 30-min block already blocks every slot covering
 * that block — quantizing therefore leaves computed slots unchanged while hiding the host's exact
 * event start/end times from the public endpoint. (Assumes whole/half-hour office tz offsets.)
 */
export function quantizeBusyToGrid(busy: BusyInterval[]): BusyInterval[] {
  const grid = SLOT_STEP_MINUTES * 60_000;
  return busy.map((b) => {
    const start = Math.floor(DateTime.fromISO(b.start).toMillis() / grid) * grid;
    const end = Math.ceil(DateTime.fromISO(b.end).toMillis() / grid) * grid;
    return { start: new Date(start).toISOString(), end: new Date(end).toISOString() };
  });
}

/**
 * Intersect busy intervals with the office-open windows. The public availability endpoint returns
 * this rather than raw FreeBusy, so it never leaks the host's calendar outside office hours. It
 * doesn't change computed slots (out-of-hours busy can't overlap an in-hours slot anyway).
 */
export function clipBusyToWindows(
  busy: BusyInterval[],
  windows: { startUtc: number; endUtc: number }[],
): BusyInterval[] {
  const clipped: BusyInterval[] = [];
  for (const b of busy) {
    const bStart = DateTime.fromISO(b.start).toMillis();
    const bEnd = DateTime.fromISO(b.end).toMillis();
    for (const w of windows) {
      const start = Math.max(bStart, w.startUtc);
      const end = Math.min(bEnd, w.endUtc);
      if (start < end) {
        clipped.push({ start: new Date(start).toISOString(), end: new Date(end).toISOString() });
      }
    }
  }
  return clipped;
}
