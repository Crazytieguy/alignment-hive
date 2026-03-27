import { query } from "./_generated/server";
import { hasCurrentAgreement } from "./lib/agreement";

/** Lightweight auth info for the frontend route guard. */
export const getAuthInfo = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return null;
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      return { hasDataAccess: false, hasAgreed: false };
    }

    const hasDataAccess = user.hasDataAccess ?? false;
    const hasAgreed = hasDataAccess
      ? await hasCurrentAgreement(ctx, user._id)
      : false;

    return { hasDataAccess, hasAgreed };
  },
});
