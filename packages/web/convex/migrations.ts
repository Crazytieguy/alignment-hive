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

/** Patch a single session: set userDocId and clear legacy userId. */
export const patchOneSession = internalMutation({
  args: {
    sessionId: v.id("sessions"),
    userDocId: v.id("users"),
  },
  handler: async (ctx, { sessionId, userDocId }) => {
    await ctx.db.patch(sessionId, { userDocId, userId: undefined });
  },
});

/** Create user records for orphaned workOS IDs found in sessions. */
export const backfillUsersFromWorkOS = internalAction({
  args: {},
  handler: async (ctx) => {
    const orphanedIds = (await ctx.runQuery(
      internal.migrations.orphanedWorkosIds,
    )) as string[];

    if (orphanedIds.length === 0) return { created: 0, failed: [] };

    const workosApiKey = process.env.WORKOS_API_KEY;
    if (!workosApiKey) throw new Error("WORKOS_API_KEY not set");

    const created: string[] = [];
    const failed: Array<{ id: string; reason: string }> = [];

    for (const workosId of orphanedIds) {
      const resp = await fetch(
        `https://api.workos.com/user_management/users/${encodeURIComponent(workosId)}`,
        { headers: { Authorization: `Bearer ${workosApiKey}` } },
      );

      if (!resp.ok) {
        failed.push({ id: workosId, reason: `HTTP ${resp.status}` });
        continue;
      }

      const user = (await resp.json()) as {
        id: string;
        email: string;
        first_name: string | null;
        last_name: string | null;
      };

      await ctx.runMutation(internal.migrations.insertUser, {
        workosId: user.id,
        email: user.email,
        firstName: user.first_name ?? undefined,
        lastName: user.last_name ?? undefined,
      });

      created.push(`${user.email} (${workosId})`);
    }

    return { created: created.length, createdUsers: created, failed };
  },
});

/** Find workOS IDs in sessions that have no matching user record. */
export const orphanedWorkosIds = internalQuery({
  args: {},
  handler: async (ctx) => {
    const sessions = await ctx.db.query("sessions").collect();
    const workosIds = new Set(
      sessions
        .filter((s) => s.userId && !s.userDocId)
        .map((s) => s.userId!),
    );

    const users = await ctx.db.query("users").collect();
    const knownWorkosIds = new Set(users.map((u) => u.workosId));

    return [...workosIds].filter((id) => !knownWorkosIds.has(id));
  },
});

/** Insert a single user record. */
export const insertUser = internalMutation({
  args: {
    workosId: v.string(),
    email: v.string(),
    firstName: v.optional(v.string()),
    lastName: v.optional(v.string()),
  },
  handler: async (ctx, { workosId, email, firstName, lastName }) => {
    return await ctx.db.insert("users", { workosId, email, firstName, lastName });
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
