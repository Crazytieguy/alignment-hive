import { describe, expect, test } from "bun:test";
import { DateTime } from "luxon";
import { OFFICES } from "./offices";
import { type Slot, generateSlots } from "./slots";

const ZONE = "America/Los_Angeles";
const mats = OFFICES.mats;
const farLabs = OFFICES["far-labs"];

function ms(iso: string): number {
  return DateTime.fromISO(iso, { zone: ZONE }).toMillis();
}
function local(slot: Slot): DateTime {
  return DateTime.fromMillis(slot.startUtc, { zone: ZONE });
}
function onLocalDate(slots: Slot[], isoDate: string): Slot[] {
  return slots
    .filter((s) => local(s).toISODate() === isoDate)
    .sort((a, b) => a.startUtc - b.startUtc);
}

describe("environment", () => {
  test("the runtime resolves the office IANA timezone (full ICU present)", () => {
    expect(DateTime.local().setZone(ZONE).isValid).toBe(true);
  });
});

describe("generateSlots — basic shape", () => {
  const now = ms("2026-06-15T00:00"); // Monday, well clear of any DST transition

  test("only emits slots on the office's configured weekdays", () => {
    const slots = generateSlots(mats, 90, [], now);
    expect(slots.length).toBeGreaterThan(0);
    for (const s of slots) {
      expect(mats.weekdays).toContain(local(s).weekday); // Tue(2) / Thu(4)
    }
  });

  test("far-labs only opens on Wednesday", () => {
    const slots = generateSlots(farLabs, 90, [], now);
    expect(slots.length).toBeGreaterThan(0);
    for (const s of slots) expect(local(s).weekday).toBe(3);
  });

  test("every slot is within office hours and ends by closing time", () => {
    const slots = generateSlots(mats, 90, [], now);
    for (const s of slots) {
      const start = local(s);
      const end = DateTime.fromMillis(s.endUtc, { zone: ZONE });
      const startMinutes = start.hour * 60 + start.minute;
      const endMinutes = end.hour * 60 + end.minute;
      expect(startMinutes).toBeGreaterThanOrEqual(10 * 60 + 30); // 10:30
      expect(endMinutes).toBeLessThanOrEqual(18 * 60); // 18:00
    }
  });

  test("back-to-back slot counts per open day depend on duration", () => {
    const date = "2026-06-16"; // a Tuesday fully inside the window
    expect(onLocalDate(generateSlots(mats, 60, [], now), date)).toHaveLength(7);
    expect(onLocalDate(generateSlots(mats, 90, [], now), date)).toHaveLength(5);
    expect(onLocalDate(generateSlots(mats, 120, [], now), date)).toHaveLength(3);
  });
});

describe("generateSlots — busy intervals (lunch / days off)", () => {
  const now = ms("2026-06-15T00:00");

  test("a busy lunch block removes only the overlapping slot, not the adjacent one", () => {
    const busy = [{ start: "2026-06-16T12:00:00-07:00", end: "2026-06-16T13:00:00-07:00" }];
    const day = onLocalDate(generateSlots(mats, 90, busy, now), "2026-06-16");
    expect(day).toHaveLength(4); // the 12:00–13:30 slot is gone; 10:30–12:00 (adjacent) stays
    for (const s of day) {
      const start = local(s);
      expect(start.hour * 60 + start.minute).not.toBe(12 * 60); // no 12:00 slot
    }
  });

  test("an all-day busy event hides the whole day", () => {
    const busy = [{ start: "2026-06-16T00:00:00-07:00", end: "2026-06-17T00:00:00-07:00" }];
    expect(onLocalDate(generateSlots(mats, 90, busy, now), "2026-06-16")).toHaveLength(0);
  });
});

describe("generateSlots — min-notice and horizon", () => {
  test("excludes slots inside the min-notice window", () => {
    const now = ms("2026-06-16T09:00"); // Tuesday 9am; same-day slots are <12h away
    const slots = generateSlots(mats, 90, [], now);
    expect(onLocalDate(slots, "2026-06-16")).toHaveLength(0);
    const from = now + 12 * 3_600_000;
    for (const s of slots) expect(s.startUtc).toBeGreaterThanOrEqual(from);
  });

  test("excludes slots beyond the booking horizon", () => {
    const now = ms("2026-06-15T00:00");
    const to = now + 21 * 24 * 3_600_000;
    for (const s of generateSlots(mats, 90, [], now)) {
      expect(s.endUtc).toBeLessThanOrEqual(to);
    }
  });

  test("horizon containment: a slot whose tail crosses the horizon is dropped", () => {
    // now chosen so the horizon (now + 21 days) lands at 13:30 PT on an open Tuesday.
    const now = ms("2026-05-26T13:30");
    const day = onLocalDate(generateSlots(mats, 90, [], now), "2026-06-16");
    expect(day).toHaveLength(2); // 10:30–12:00 and 12:00–13:30 only; 13:30–15:00 crosses the horizon
    const lastEnd = DateTime.fromMillis(day[day.length - 1].endUtc, { zone: ZONE });
    expect(lastEnd.hour * 60 + lastEnd.minute).toBe(13 * 60 + 30); // last slot ends exactly at horizon
    for (const s of day) {
      const start = local(s);
      expect(start.hour * 60 + start.minute).not.toBe(13 * 60 + 30); // nothing starts at the horizon
    }
  });
});

describe("generateSlots — DST correctness", () => {
  test("spring-forward: wall-clock stays 10:30 while the UTC offset shifts", () => {
    const now = ms("2026-03-01T00:00"); // DST begins Sun 2026-03-08
    const slots = generateSlots(mats, 90, [], now);

    const beforeDst = onLocalDate(slots, "2026-03-03")[0]; // Tue, still PST
    const afterDst = onLocalDate(slots, "2026-03-10")[0]; // Tue, now PDT

    expect(local(beforeDst).hour).toBe(10);
    expect(local(beforeDst).minute).toBe(30);
    expect(local(beforeDst).offset).toBe(-480); // PST = UTC-8

    expect(local(afterDst).hour).toBe(10);
    expect(local(afterDst).minute).toBe(30);
    expect(local(afterDst).offset).toBe(-420); // PDT = UTC-7
  });

  test("fall-back: wall-clock stays 10:30 while the UTC offset shifts", () => {
    const now = ms("2026-10-22T00:00"); // DST ends Sun 2026-11-01
    const slots = generateSlots(mats, 90, [], now);

    const beforeStd = onLocalDate(slots, "2026-10-27")[0]; // Tue, still PDT
    const afterStd = onLocalDate(slots, "2026-11-03")[0]; // Tue, now PST

    expect(local(beforeStd).hour).toBe(10);
    expect(local(beforeStd).minute).toBe(30);
    expect(local(beforeStd).offset).toBe(-420); // PDT

    expect(local(afterStd).hour).toBe(10);
    expect(local(afterStd).minute).toBe(30);
    expect(local(afterStd).offset).toBe(-480); // PST
  });
});
