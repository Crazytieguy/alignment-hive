import { v } from "convex/values";
import {
  internalAction,
  internalMutation,
  internalQuery,
} from "./_generated/server";
import { internal } from "./_generated/api";
import type { Id } from "./_generated/dataModel";

/** Find sessions that have userId (workosId) but no userDocId. */
export const sessionsNeedingUserDocId = internalQuery({
  args: {},
  handler: async (ctx) => {
    const sessions = await ctx.db.query("sessions").collect();
    return sessions
      .filter((s) => s.userId && !s.userDocId)
      .map((s) => ({ _id: s._id, userId: s.userId! }));
  },
});

/** Get all users for the migration lookup. */
export const allUsersForMigration = internalQuery({
  args: {},
  handler: async (ctx) => {
    const users = await ctx.db.query("users").collect();
    return users.map((u) => ({ _id: u._id, workosId: u.workosId }));
  },
});

/** Patch a single session with the resolved userDocId. */
export const patchOneSession = internalMutation({
  args: {
    sessionId: v.id("sessions"),
    userDocId: v.id("users"),
  },
  handler: async (ctx, { sessionId, userDocId }) => {
    await ctx.db.patch(sessionId, { userDocId });
  },
});

/** Backfill userDocId on all sessions that only have the legacy userId (workosId). */
export const backfillUserDocId = internalAction({
  args: {},
  handler: async (
    ctx,
  ): Promise<{ updated: number; skipped: number; total: number }> => {
    const sessions = (await ctx.runQuery(
      internal.migrations.sessionsNeedingUserDocId,
    )) as Array<{ _id: Id<"sessions">; userId: string }>;

    const users = (await ctx.runQuery(
      internal.migrations.allUsersForMigration,
    )) as Array<{ _id: Id<"users">; workosId: string }>;
    const workosToDocId = new Map(users.map((u) => [u.workosId, u._id]));

    let updated = 0;
    let skipped = 0;

    for (const session of sessions) {
      const docId = workosToDocId.get(session.userId);
      if (!docId) {
        skipped++;
        continue;
      }
      await ctx.runMutation(internal.migrations.patchOneSession, {
        sessionId: session._id,
        userDocId: docId,
      });
      updated++;
    }

    return { updated, skipped, total: sessions.length };
  },
});
