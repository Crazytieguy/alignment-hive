// Single-use state for consulting-feedback tokens: claim (pending) -> confirm (redeemed), or
// release on a failure that wrote nothing. Called only by the web server (src/lib/feedback/server.ts).
import { v } from "convex/values";
import { mutation, query } from "./_generated/server";

// A pending claim older than this may be re-claimed: it means a submission died between claim
// and confirm (e.g. a Sheets timeout), and we prefer a small at-least-once duplicate window over
// permanently burning the fellow's link.
export const CLAIM_TTL_MS = 10 * 60 * 1000;

/**
 * These functions are publicly reachable (all Convex functions are), so each one requires the
 * feedback service secret, known only to the web server routes and this deployment's env. Token
 * HMAC verification itself happens in the web server — the only thing Convex ever sees is an
 * opaque hash.
 */
function requireServiceSecret(provided: string): void {
  const expected = process.env.FEEDBACK_TOKEN_SECRET;
  if (!expected || !constantTimeEquals(provided, expected)) throw new Error("Unauthorized");
}

/**
 * String equality that reads every character of `expected` whatever `provided` is, so the time it
 * takes doesn't reveal how much of a guess matched. node:crypto's timingSafeEqual is only available
 * in Node actions, and these are queries and mutations.
 */
function constantTimeEquals(provided: string, expected: string): boolean {
  let diff = provided.length ^ expected.length;
  for (let i = 0; i < expected.length; i++) {
    // Past the end of `provided`, charCodeAt is NaN, which ^ treats as 0.
    diff |= provided.charCodeAt(i) ^ expected.charCodeAt(i);
  }
  return diff === 0;
}

export const claim = mutation({
  args: { tokenHash: v.string(), serviceSecret: v.string() },
  handler: async (ctx, { tokenHash, serviceSecret }) => {
    requireServiceSecret(serviceSecret);
    const now = Date.now();
    const existing = await ctx.db
      .query("feedbackTokens")
      .withIndex("by_token_hash", (q) => q.eq("tokenHash", tokenHash))
      .unique();
    if (!existing) {
      await ctx.db.insert("feedbackTokens", {
        tokenHash,
        status: "pending",
        claimedAt: now,
      });
      return { ok: true as const };
    }
    if (existing.status === "redeemed")
      return { ok: false as const, reason: "redeemed" as const };
    if (now - existing.claimedAt < CLAIM_TTL_MS)
      return { ok: false as const, reason: "pending" as const };
    await ctx.db.patch(existing._id, { claimedAt: now });
    return { ok: true as const };
  },
});

export const confirm = mutation({
  args: { tokenHash: v.string(), serviceSecret: v.string() },
  handler: async (ctx, { tokenHash, serviceSecret }) => {
    requireServiceSecret(serviceSecret);
    const existing = await ctx.db
      .query("feedbackTokens")
      .withIndex("by_token_hash", (q) => q.eq("tokenHash", tokenHash))
      .unique();
    if (!existing)
      throw new Error("Cannot confirm a token that was never claimed");
    await ctx.db.patch(existing._id, { status: "redeemed" });
  },
});

/** Undo a claim after a definitive pre-write failure, so the link stays usable. */
export const release = mutation({
  args: { tokenHash: v.string(), serviceSecret: v.string() },
  handler: async (ctx, { tokenHash, serviceSecret }) => {
    requireServiceSecret(serviceSecret);
    const existing = await ctx.db
      .query("feedbackTokens")
      .withIndex("by_token_hash", (q) => q.eq("tokenHash", tokenHash))
      .unique();
    if (existing && existing.status === "pending")
      await ctx.db.delete(existing._id);
  },
});

export const status = query({
  args: { tokenHash: v.string(), serviceSecret: v.string() },
  handler: async (ctx, { tokenHash, serviceSecret }) => {
    requireServiceSecret(serviceSecret);
    const existing = await ctx.db
      .query("feedbackTokens")
      .withIndex("by_token_hash", (q) => q.eq("tokenHash", tokenHash))
      .unique();
    return existing?.status === "redeemed"
      ? ("redeemed" as const)
      : ("valid" as const);
  },
});
