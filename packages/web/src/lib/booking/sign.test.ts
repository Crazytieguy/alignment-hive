import { describe, expect, test } from "bun:test";

process.env.BOOKING_SIGNING_SECRET = "test-secret-for-unit-tests";
const { buildCancelUrl, signEventId, verifyEventSignature } = await import("./sign");

describe("cancel-link signing", () => {
  test("a valid signature verifies", () => {
    const id = "abc123";
    expect(verifyEventSignature(id, signEventId(id))).toBe(true);
  });

  test("a tampered signature is rejected", () => {
    const id = "abc123";
    const sig = signEventId(id);
    expect(verifyEventSignature(id, sig.slice(0, -1) + (sig.endsWith("0") ? "1" : "0"))).toBe(false);
    expect(verifyEventSignature(id, "")).toBe(false);
    expect(verifyEventSignature("different-id", sig)).toBe(false);
  });

  test("buildCancelUrl embeds the event id and a matching signature", () => {
    process.env.SITE_URL = "https://alignment-hive.com";
    const url = new URL(buildCancelUrl("evt-1"));
    expect(url.pathname).toBe("/book/cancel");
    expect(url.searchParams.get("e")).toBe("evt-1");
    expect(verifyEventSignature("evt-1", url.searchParams.get("sig")!)).toBe(true);
  });
});
