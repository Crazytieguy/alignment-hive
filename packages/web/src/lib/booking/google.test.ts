import { describe, expect, test } from "bun:test";
import { parseFreeBusyResponse, randomEventId } from "./google";

describe("parseFreeBusyResponse", () => {
  test("returns the primary calendar's busy intervals", () => {
    const busy = parseFreeBusyResponse({
      kind: "calendar#freeBusy", // extra fields are tolerated
      calendars: {
        primary: { busy: [{ start: "2026-06-16T19:00:00Z", end: "2026-06-16T20:00:00Z" }] },
      },
    });
    expect(busy).toEqual([{ start: "2026-06-16T19:00:00Z", end: "2026-06-16T20:00:00Z" }]);
  });

  test("treats an empty calendar as no busy intervals", () => {
    expect(parseFreeBusyResponse({ calendars: { primary: {} } })).toEqual([]);
  });

  test("fails closed when the primary calendar reports errors", () => {
    expect(() =>
      parseFreeBusyResponse({ calendars: { primary: { errors: [{ reason: "notFound" }] } } }),
    ).toThrow();
  });

  test("fails closed when the primary calendar is missing", () => {
    expect(() => parseFreeBusyResponse({ calendars: {} })).toThrow();
  });
});

describe("randomEventId", () => {
  test("is a valid Google event id and unique per call", () => {
    const id = randomEventId();
    expect(id).toMatch(/^[0-9a-v]{5,1024}$/); // Google's allowed id charset (base32hex)
    expect(id).toMatch(/^[0-9a-f]{64}$/); // 32 random bytes as hex
    expect(randomEventId()).not.toBe(id);
  });
});
