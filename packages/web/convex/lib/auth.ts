import type { QueryCtx, MutationCtx } from "../_generated/server";
import { hasCurrentAgreement } from "./agreement";

/**
 * Check if the current user is authorized to view shared session data.
 * Returns null if not authenticated (e.g., during SSR/loading).
 * Throws if authenticated but not authorized.
 *
 * Requires: hasDataAccess flag on users table + signed current agreement version.
 */
export async function requireAuthorized(
  ctx: QueryCtx | MutationCtx,
): Promise<{
  identity: NonNullable<
    Awaited<ReturnType<typeof ctx.auth.getUserIdentity>>
  >;
} | null> {
  const identity = await ctx.auth.getUserIdentity();
  if (!identity) {
    return null;
  }

  const user = await ctx.db
    .query("users")
    .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
    .first();

  if (!user?.hasDataAccess) {
    throw new Error("Not authorized");
  }

  if (!(await hasCurrentAgreement(ctx, user._id))) {
    throw new Error("Agreement required");
  }

  return { identity };
}
