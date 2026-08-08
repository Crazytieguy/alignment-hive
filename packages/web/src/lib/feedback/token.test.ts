import { describe, expect, test } from "bun:test";

process.env.FEEDBACK_TOKEN_SECRET = "test-secret-for-unit-tests";
const { buildFeedbackUrl, hashTokenId, newFeedbackToken, verifyFeedbackToken } =
  await import("./token");

describe("feedback tokens", () => {
  test("a freshly generated token verifies and yields its id", () => {
    const token = newFeedbackToken();
    const id = verifyFeedbackToken(token);
    expect(id).not.toBeNull();
    expect(token.startsWith(`${id}.`)).toBe(true);
  });

  test("tampered tokens are rejected", () => {
    const token = newFeedbackToken();
    const [id, sig] = token.split(".");
    expect(verifyFeedbackToken(`${id}.${sig.slice(0, -1)}0`)).toBeNull();
    expect(verifyFeedbackToken(`${id}x.${sig}`)).toBeNull();
    expect(verifyFeedbackToken(id)).toBeNull();
    expect(verifyFeedbackToken(`${id}.`)).toBeNull();
    expect(verifyFeedbackToken(`.${sig}`)).toBeNull();
    expect(verifyFeedbackToken("")).toBeNull();
  });

  test("tokens signed with a different secret are rejected", () => {
    const token = newFeedbackToken();
    process.env.FEEDBACK_TOKEN_SECRET = "another-secret";
    expect(verifyFeedbackToken(token)).toBeNull();
    process.env.FEEDBACK_TOKEN_SECRET = "test-secret-for-unit-tests";
    expect(verifyFeedbackToken(token)).not.toBeNull();
  });

  test("token ids hash deterministically and tokens are unique", () => {
    const a = verifyFeedbackToken(newFeedbackToken())!;
    const b = verifyFeedbackToken(newFeedbackToken())!;
    expect(a).not.toBe(b);
    expect(hashTokenId(a)).toBe(hashTokenId(a));
    expect(hashTokenId(a)).not.toBe(hashTokenId(b));
    expect(hashTokenId(a)).toMatch(/^[0-9a-f]{64}$/);
  });

  test("buildFeedbackUrl embeds the token", () => {
    process.env.SITE_URL = "https://alignment-hive.com";
    const token = newFeedbackToken();
    const url = new URL(buildFeedbackUrl(token));
    expect(url.pathname).toBe("/feedback/mats");
    expect(url.searchParams.get("t")).toBe(token);
  });
});
