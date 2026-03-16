import type { QueryCtx, MutationCtx } from "../_generated/server";
import { getAdminEmails } from "./admin";

export type AuthRole = "admin" | "reader";

/**
 * Check if the current user is authorized to view shared session data.
 * Returns null if not authenticated (e.g., during SSR/loading).
 * Throws if authenticated but has neither admin nor reader access.
 *
 * Admin: email in ADMIN_EMAILS env var (sees all sessions).
 * Reader: hasDataAccess flag on users table (sees only consented sessions).
 */
export async function requireAuthorized(
  ctx: QueryCtx | MutationCtx,
): Promise<{ identity: NonNullable<Awaited<ReturnType<typeof ctx.auth.getUserIdentity>>>; role: AuthRole } | null> {
  const identity = await ctx.auth.getUserIdentity();
  if (!identity) {
    return null;
  }

  // Check admin first
  const adminEmails = getAdminEmails();
  if (identity.email && adminEmails.includes(identity.email)) {
    return { identity, role: "admin" };
  }

  // Check reader (hasDataAccess on users table)
  const user = await ctx.db
    .query("users")
    .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
    .first();

  if (user?.hasDataAccess) {
    return { identity, role: "reader" };
  }

  throw new Error("Not authorized");
}
