import { query } from "./_generated/server";
import { CURRENT_AGREEMENT_VERSION } from "./lib/agreement";

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

    let hasAgreed = false;
    if (hasDataAccess) {
      const agreements = await ctx.db
        .query("dataAccessorAgreements")
        .withIndex("by_user_id", (q) => q.eq("userId", user._id))
        .collect();
      hasAgreed = agreements.some(
        (a) => a.agreementVersion === CURRENT_AGREEMENT_VERSION,
      );
    }

    return { hasDataAccess, hasAgreed };
  },
});
