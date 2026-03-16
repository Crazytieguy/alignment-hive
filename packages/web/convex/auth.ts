import { query } from "./_generated/server";
import { getAdminEmails } from "./lib/admin";

/** Lightweight auth info for the frontend route guard. */
export const getAuthInfo = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return null;
    }

    const adminEmails = getAdminEmails();
    const isAdmin =
      !!identity.email && adminEmails.includes(identity.email);

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    return {
      isAdmin,
      hasDataAccess: user?.hasDataAccess ?? false,
    };
  },
});
