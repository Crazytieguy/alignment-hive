import { afterEach, describe, expect, test } from "bun:test";
import { GoogleApiError } from "@/lib/booking/google";
import { appendSheetRow } from "./sheets";

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function mockFetch(responses: (() => Response | Error)[]): () => number {
  let calls = 0;
  globalThis.fetch = (async () => {
    const next = responses[Math.min(calls, responses.length - 1)];
    calls++;
    const result = next();
    if (result instanceof Error) throw result;
    return result;
  }) as unknown as typeof fetch;
  return () => calls;
}

const ok = () => new Response("{}", { status: 200 });
const unavailable = () =>
  new Response('{"error":{"code":503}}', { status: 503 });

describe("appendSheetRow", () => {
  test("succeeds first try without retrying", async () => {
    const calls = mockFetch([ok]);
    await appendSheetRow("tok", "sheet", "responses!A:D", ["a"], [0, 0]);
    expect(calls()).toBe(1);
  });

  test("retries a 503 and succeeds", async () => {
    const calls = mockFetch([unavailable, ok]);
    await appendSheetRow("tok", "sheet", "responses!A:D", ["a"], [0, 0]);
    expect(calls()).toBe(2);
  });

  test("retries a network error and succeeds", async () => {
    const calls = mockFetch([() => new Error("socket hang up"), ok]);
    await appendSheetRow("tok", "sheet", "responses!A:D", ["a"], [0, 0]);
    expect(calls()).toBe(2);
  });

  test("throws the last error once retries are exhausted", async () => {
    const calls = mockFetch([unavailable]);
    await expect(
      appendSheetRow("tok", "sheet", "responses!A:D", ["a"], [0, 0]),
    ).rejects.toBeInstanceOf(GoogleApiError);
    expect(calls()).toBe(3);
  });

  test("does not retry a definitive 4xx rejection", async () => {
    const calls = mockFetch([
      () => new Response("bad range", { status: 400 }),
      ok,
    ]);
    await expect(
      appendSheetRow("tok", "sheet", "responses!A:D", ["a"], [0, 0]),
    ).rejects.toMatchObject({ status: 400 });
    expect(calls()).toBe(1);
  });
});
