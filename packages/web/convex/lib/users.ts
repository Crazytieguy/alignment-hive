import { ConvexError } from "convex/values";
import type { MutationCtx } from "../_generated/server";
import type { UserIdentity } from "convex/server";
import type { Id } from "../_generated/dataModel";

/** Upsert user record with latest identity info from JWT. Returns the user's document ID. */
export async function upsertUser(
  ctx: MutationCtx,
  identity: UserIdentity,
): Promise<Id<"users">> {
  const userId = identity.subject;
  const givenName = (identity as Record<string, unknown>)["given_name"] as
    | string
    | undefined;
  const familyName = (identity as Record<string, unknown>)["family_name"] as
    | string
    | undefined;

  const existingUser = await ctx.db
    .query("users")
    .withIndex("by_workos_id", (q) => q.eq("workosId", userId))
    .first();

  if (existingUser) {
    if (
      existingUser.firstName !== givenName ||
      existingUser.lastName !== familyName ||
      existingUser.email !== identity.email
    ) {
      await ctx.db.patch(existingUser._id, {
        email: identity.email ?? existingUser.email,
        firstName: givenName ?? existingUser.firstName,
        lastName: familyName ?? existingUser.lastName,
      });
    }
    return existingUser._id;
  }

  if (!identity.email) {
    throw new ConvexError("User has no email — cannot create account");
  }

  return await ctx.db.insert("users", {
    workosId: userId,
    email: identity.email,
    firstName: givenName,
    lastName: familyName,
  });
}
