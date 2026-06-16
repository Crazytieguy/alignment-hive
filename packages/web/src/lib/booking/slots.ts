import { DateTime, Interval } from "luxon";
import { HORIZON_DAYS, MIN_NOTICE_HOURS, type OfficeConfig } from "./offices";

const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

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
 * A slot is excluded if it overlaps any `busy` interval (this is how lunch, days off, and
 * existing meetings disappear — Yoav blocks them on his calendar) or falls outside the
 * [now + MIN_NOTICE_HOURS, now + HORIZON_DAYS] window.
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
    if (office.weekdays.includes(day.weekday)) {
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
        slotStart = slotEnd; // back-to-back slots
      }
    }
    day = day.plus({ days: 1 });
  }

  return slots;
}
