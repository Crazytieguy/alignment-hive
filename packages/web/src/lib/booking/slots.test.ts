import { describe, expect, test } from "bun:test";
import { DateTime } from "luxon";
import { OFFICES, type OfficeConfig, isOfficeOpenOn } from "./offices";
import {
  type Slot,
  clipBusyToWindows,
  generateSlots,
  officeOpenWindows,
  quantizeBusyToGrid,
} from "./slots";

const ZONE = "America/Los_Angeles";
// Fixture configs: the mechanics tests below must not churn whenever the real office
// schedules in OFFICES change; only the "real office schedule" test reads the live config.
const office: OfficeConfig = {
  label: "Test office",
  timezone: ZONE,
  weekdays: [2, 4], // Tue, Thu
  start: "10:30",
  end: "18:00",
};
const singleDay: OfficeConfig = { ...office, weekdays: [3] }; // Wed

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
    const slots = generateSlots(office, 90, [], now);
    expect(slots.length).toBeGreaterThan(0);
    for (const s of slots) {
      expect(office.weekdays).toContain(local(s).weekday); // Tue(2) / Thu(4)
    }
  });

  test("a single-weekday office only opens on that day", () => {
    const slots = generateSlots(singleDay, 90, [], now);
    expect(slots.length).toBeGreaterThan(0);
    for (const s of slots) expect(local(s).weekday).toBe(3);
  });

  test("every slot is within office hours and ends by closing time", () => {
    const slots = generateSlots(office, 90, [], now);
    for (const s of slots) {
      const start = local(s);
      const end = DateTime.fromMillis(s.endUtc, { zone: ZONE });
      const startMinutes = start.hour * 60 + start.minute;
      const endMinutes = end.hour * 60 + end.minute;
      expect(startMinutes).toBeGreaterThanOrEqual(10 * 60 + 30); // 10:30
      expect(endMinutes).toBeLessThanOrEqual(18 * 60); // 18:00
    }
  });

  test("offers 30-min-increment starts that fit before closing", () => {
    const date = "2026-06-16"; // a Tuesday fully inside the window
    // 10:30–18:00 on 30-min steps: 60m ends by 17:00 (14 starts), 90m by 16:30 (13), 120m by 16:00 (12)
    expect(onLocalDate(generateSlots(office, 60, [], now), date)).toHaveLength(14);
    expect(onLocalDate(generateSlots(office, 90, [], now), date)).toHaveLength(13);
    expect(onLocalDate(generateSlots(office, 120, [], now), date)).toHaveLength(12);
  });

  test("consecutive starts are 30 minutes apart", () => {
    const day = onLocalDate(generateSlots(office, 90, [], now), "2026-06-16");
    expect(day[1].startUtc - day[0].startUtc).toBe(30 * 60_000);
  });
});

describe("generateSlots — busy intervals (lunch / days off)", () => {
  const now = ms("2026-06-15T00:00");

  test("a busy lunch block removes the overlapping starts, not the adjacent ones", () => {
    const busy = [{ start: "2026-06-16T12:00:00-07:00", end: "2026-06-16T13:00:00-07:00" }];
    const day = onLocalDate(generateSlots(office, 90, busy, now), "2026-06-16");
    // 13 starts minus the four 90-min starts overlapping 12:00–13:00 (11:00, 11:30, 12:00, 12:30)
    expect(day).toHaveLength(9);
    for (const s of day) {
      const start = local(s);
      const mins = start.hour * 60 + start.minute;
      expect([11 * 60, 11 * 60 + 30, 12 * 60, 12 * 60 + 30]).not.toContain(mins);
    }
    expect(local(day[0]).hour * 60 + local(day[0]).minute).toBe(10 * 60 + 30); // 10:30 (ends 12:00) stays
  });

  test("an all-day busy event hides the whole day", () => {
    const busy = [{ start: "2026-06-16T00:00:00-07:00", end: "2026-06-17T00:00:00-07:00" }];
    expect(onLocalDate(generateSlots(office, 90, busy, now), "2026-06-16")).toHaveLength(0);
  });
});

describe("generateSlots — min-notice and horizon", () => {
  test("no min-notice buffer: same-day slots stay available, already-started slots don't", () => {
    const now = ms("2026-06-16T12:15"); // Tuesday mid-day
    const slots = generateSlots(office, 90, [], now);
    const sameDay = onLocalDate(slots, "2026-06-16");
    // 12:30–16:30 starts remain (10:30–12:00 have already begun)
    expect(sameDay.length).toBeGreaterThan(0);
    expect(local(sameDay[0]).hour * 60 + local(sameDay[0]).minute).toBe(12 * 60 + 30);
    for (const s of slots) expect(s.startUtc).toBeGreaterThanOrEqual(now);
  });

  test("excludes slots beyond the booking horizon", () => {
    const now = ms("2026-06-15T00:00");
    const to = now + 21 * 24 * 3_600_000;
    for (const s of generateSlots(office, 90, [], now)) {
      expect(s.endUtc).toBeLessThanOrEqual(to);
    }
  });

  test("horizon containment: a slot whose tail crosses the horizon is dropped", () => {
    // now chosen so the horizon (now + 21 days) lands at 13:30 PT on an open Tuesday.
    const now = ms("2026-05-26T13:30");
    const day = onLocalDate(generateSlots(office, 90, [], now), "2026-06-16");
    // 90-min starts ending by 13:30: 10:30, 11:00, 11:30, 12:00 (12:30 would end 14:00, past the horizon)
    expect(day).toHaveLength(4);
    const lastEnd = DateTime.fromMillis(day[day.length - 1].endUtc, { zone: ZONE });
    expect(lastEnd.hour * 60 + lastEnd.minute).toBe(13 * 60 + 30); // last slot ends exactly at horizon
  });
});

describe("clipBusyToWindows — keeps the public endpoint from leaking the host calendar", () => {
  const now = ms("2026-06-15T00:00");
  const windows = officeOpenWindows(office, now);

  test("drops busy outside office hours/days", () => {
    const busy = [
      { start: "2026-06-20T20:00:00-07:00", end: "2026-06-20T22:00:00-07:00" }, // Saturday evening
      { start: "2026-06-16T03:00:00-07:00", end: "2026-06-16T04:00:00-07:00" }, // Tue 3am, before open
    ];
    expect(clipBusyToWindows(busy, windows)).toHaveLength(0);
  });

  test("keeps and clips busy that overlaps office hours", () => {
    const busy = [{ start: "2026-06-16T09:00:00-07:00", end: "2026-06-16T13:00:00-07:00" }];
    const clipped = clipBusyToWindows(busy, windows);
    expect(clipped).toHaveLength(1);
    expect(DateTime.fromISO(clipped[0].start, { zone: ZONE }).toFormat("H:mm")).toBe("10:30"); // clipped to open
    expect(DateTime.fromISO(clipped[0].end, { zone: ZONE }).toFormat("H:mm")).toBe("13:00");
  });

  test("clipping doesn't change the generated slots", () => {
    const busy = [{ start: "2026-06-16T12:00:00-07:00", end: "2026-06-16T13:00:00-07:00" }];
    const clipped = clipBusyToWindows(busy, windows);
    const fromRaw = generateSlots(office, 90, busy, now).map((s) => s.startUtc);
    const fromClipped = generateSlots(office, 90, clipped, now).map((s) => s.startUtc);
    expect(fromClipped).toEqual(fromRaw);
  });
});

describe("quantizeBusyToGrid — hides exact event times without changing slots", () => {
  const now = ms("2026-06-15T00:00");

  test("rounds busy outward to the 30-min grid", () => {
    const q = quantizeBusyToGrid([
      { start: "2026-06-16T11:15:00-07:00", end: "2026-06-16T11:35:00-07:00" },
    ]);
    expect(DateTime.fromISO(q[0].start, { zone: ZONE }).toFormat("H:mm")).toBe("11:00");
    expect(DateTime.fromISO(q[0].end, { zone: ZONE }).toFormat("H:mm")).toBe("12:00");
  });

  test("doesn't change the generated slots", () => {
    const busy = [{ start: "2026-06-16T11:15:00-07:00", end: "2026-06-16T11:35:00-07:00" }];
    const raw = generateSlots(office, 90, busy, now).map((s) => s.startUtc);
    const quantized = generateSlots(office, 90, quantizeBusyToGrid(busy), now).map((s) => s.startUtc);
    expect(quantized).toEqual(raw);
  });
});

describe("schedule override", () => {
  const now = ms("2026-06-08T00:00"); // Monday, one week before the override window
  const overridden: OfficeConfig = {
    ...office,
    weekdays: [4], // Thu outside the override window
    override: { from: "2026-06-15", until: "2026-06-18", weekdays: [2] }, // Mon–Thu: Tue only
  };

  test("isOfficeOpenOn applies the override between from and until (both inclusive), the base outside", () => {
    const day = (iso: string) => DateTime.fromISO(iso, { zone: ZONE });
    expect(isOfficeOpenOn(overridden, day("2026-06-09"))).toBe(false); // Tue before `from`: base weekdays
    expect(isOfficeOpenOn(overridden, day("2026-06-11"))).toBe(true); // Thu before `from`: base weekdays
    expect(isOfficeOpenOn(overridden, day("2026-06-16"))).toBe(true); // Tue inside override
    expect(isOfficeOpenOn(overridden, day("2026-06-18"))).toBe(false); // Thu on `until` itself: still overridden
    expect(isOfficeOpenOn(overridden, day("2026-06-23"))).toBe(false); // Tue after: base weekdays
    expect(isOfficeOpenOn(overridden, day("2026-06-25"))).toBe(true); // Thu after: base weekdays
  });

  test("generateSlots applies the override window and the base weekdays around it", () => {
    const slots = generateSlots(overridden, 90, [], now);
    expect(onLocalDate(slots, "2026-06-09")).toHaveLength(0); // Tue before override: closed
    expect(onLocalDate(slots, "2026-06-11").length).toBeGreaterThan(0); // Thu before override
    expect(onLocalDate(slots, "2026-06-16").length).toBeGreaterThan(0); // Tue, override week
    expect(onLocalDate(slots, "2026-06-18")).toHaveLength(0); // Thu, override week: closed
    expect(onLocalDate(slots, "2026-06-23")).toHaveLength(0); // Tue after override: closed
    expect(onLocalDate(slots, "2026-06-25").length).toBeGreaterThan(0); // Thu after override
  });

  test("officeOpenWindows applies the override the same way", () => {
    const days = officeOpenWindows(overridden, now).map((w) =>
      DateTime.fromMillis(w.startUtc, { zone: ZONE }).toISODate(),
    );
    expect(days).not.toContain("2026-06-09");
    expect(days).toContain("2026-06-11");
    expect(days).toContain("2026-06-16");
    expect(days).not.toContain("2026-06-18");
    expect(days).not.toContain("2026-06-23");
    expect(days).toContain("2026-06-25");
  });

  // Pins the 2026-07 schedule; delete together with the mats `override` once it has passed.
  test("real mats schedule: Tue+Thu, plus Fri the week of 2026-07-13", () => {
    const slots = generateSlots(OFFICES.mats, 90, [], ms("2026-07-06T00:00"));
    expect(onLocalDate(slots, "2026-07-07").length).toBeGreaterThan(0); // Tue this week
    expect(onLocalDate(slots, "2026-07-09").length).toBeGreaterThan(0); // Thu this week
    expect(onLocalDate(slots, "2026-07-10")).toHaveLength(0); // Fri this week: closed
    // Tue 2026-07-14 stays open in config; Yoav closes it with a calendar block.
    expect(onLocalDate(slots, "2026-07-14").length).toBeGreaterThan(0);
    expect(onLocalDate(slots, "2026-07-16").length).toBeGreaterThan(0); // Thu next week
    expect(onLocalDate(slots, "2026-07-17").length).toBeGreaterThan(0); // Fri next week: open
    expect(onLocalDate(slots, "2026-07-21").length).toBeGreaterThan(0); // Tue the week after
    expect(onLocalDate(slots, "2026-07-24")).toHaveLength(0); // Fri the week after: closed
  });

  test("real far-labs schedule: Wednesdays", () => {
    const slots = generateSlots(OFFICES["far-labs"], 90, [], ms("2026-07-06T00:00"));
    expect(slots.length).toBeGreaterThan(0);
    for (const s of slots) expect(local(s).weekday).toBe(3);
  });
});

describe("generateSlots — DST correctness", () => {
  test("spring-forward: wall-clock stays 10:30 while the UTC offset shifts", () => {
    const now = ms("2026-03-01T00:00"); // DST begins Sun 2026-03-08
    const slots = generateSlots(office, 90, [], now);

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
    const slots = generateSlots(office, 90, [], now);

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
