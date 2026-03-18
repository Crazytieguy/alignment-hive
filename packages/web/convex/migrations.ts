/**
 * Migration: populate directory/gitRemote fields from legacy `project` field.
 *
 * Classifies existing `project` values using the shared heuristic:
 * starts with "/" → directory, else → gitRemote.
 *
 * Run via dashboard: `npx convex run migrations:migrateProjectIdentifiers`
 */
import { v } from "convex/values";
import { internalMutation, internalAction } from "./_generated/server";
import { internal } from "./_generated/api";
import { classifyLegacyProject } from "@alignment-hive/session-data";

const BATCH_SIZE = 100;

/** Migrate a page of projectConsent records. Uses Convex pagination to avoid full table scans. */
export const migrateProjectConsentBatch = internalMutation({
  args: { cursor: v.optional(v.string()) },
  handler: async (ctx, { cursor }) => {
    const page = await ctx.db
      .query("projectConsent")
      .paginate({ numItems: BATCH_SIZE, cursor: cursor ?? null });

    let migrated = 0;
    for (const record of page.page) {
      // Skip if already migrated (has directory or gitRemote)
      if ('directory' in record && record.directory) continue;
      if ('gitRemote' in record && record.gitRemote) continue;
      if (!('project' in record) || !record.project) continue;

      const ids = classifyLegacyProject(record.project);
      await ctx.db.patch(record._id, {
        ...(ids.directory ? { directory: ids.directory } : {}),
        ...(ids.gitRemote ? { gitRemote: ids.gitRemote } : {}),
      });
      migrated++;
    }
    return {
      migrated,
      isDone: page.isDone,
      cursor: page.continueCursor,
    };
  },
});

/** Migrate a page of session records. Uses Convex pagination to avoid full table scans. */
export const migrateSessionsBatch = internalMutation({
  args: { cursor: v.optional(v.string()) },
  handler: async (ctx, { cursor }) => {
    const page = await ctx.db
      .query("sessions")
      .paginate({ numItems: BATCH_SIZE, cursor: cursor ?? null });

    let migrated = 0;
    for (const record of page.page) {
      if (record.directory || record.gitRemote) continue;
      if (!record.project) continue;

      const ids = classifyLegacyProject(record.project);
      await ctx.db.patch(record._id, {
        ...(ids.directory ? { directory: ids.directory } : {}),
        ...(ids.gitRemote ? { gitRemote: ids.gitRemote } : {}),
      });
      migrated++;
    }
    return {
      migrated,
      isDone: page.isDone,
      cursor: page.continueCursor,
    };
  },
});

/** Run the full migration. Pages through both tables. */
export const migrateProjectIdentifiers = internalAction({
  args: {},
  handler: async (ctx): Promise<{ projectConsent: number; sessions: number }> => {
    let totalConsent = 0;
    let totalSessions = 0;

    type BatchResult = { migrated: number; isDone: boolean; cursor: string };

    // Migrate projectConsent
    let cursor: string | undefined;
    while (true) {
      const result: BatchResult = await ctx.runMutation(
        internal.migrations.migrateProjectConsentBatch,
        { cursor },
      );
      totalConsent += result.migrated;
      if (result.isDone) break;
      cursor = result.cursor;
    }

    // Migrate sessions
    cursor = undefined;
    while (true) {
      const result: BatchResult = await ctx.runMutation(
        internal.migrations.migrateSessionsBatch,
        { cursor },
      );
      totalSessions += result.migrated;
      if (result.isDone) break;
      cursor = result.cursor;
    }

    return { projectConsent: totalConsent, sessions: totalSessions };
  },
});

// --- Migration: lowercase gitRemote ---

/** Lowercase gitRemote in a batch of projectConsent records. */
export const lowercaseProjectConsentBatch = internalMutation({
  args: { cursor: v.optional(v.string()) },
  handler: async (ctx, { cursor }) => {
    const page = await ctx.db
      .query("projectConsent")
      .paginate({ numItems: BATCH_SIZE, cursor: cursor ?? null });

    let migrated = 0;
    for (const doc of page.page) {
      const remote = 'gitRemote' in doc ? doc.gitRemote : undefined;
      if (remote && remote !== remote.toLowerCase()) {
        await ctx.db.patch(doc._id, {
          gitRemote: remote.toLowerCase(),
        });
        migrated++;
      }
    }

    return {
      migrated,
      isDone: page.isDone,
      cursor: page.continueCursor,
    };
  },
});

/** Lowercase gitRemote in a batch of session records. */
export const lowercaseSessionsBatch = internalMutation({
  args: { cursor: v.optional(v.string()) },
  handler: async (ctx, { cursor }) => {
    const page = await ctx.db
      .query("sessions")
      .paginate({ numItems: BATCH_SIZE, cursor: cursor ?? null });

    let migrated = 0;
    for (const doc of page.page) {
      if (doc.gitRemote && doc.gitRemote !== doc.gitRemote.toLowerCase()) {
        await ctx.db.patch(doc._id, {
          gitRemote: doc.gitRemote.toLowerCase(),
        });
        migrated++;
      }
    }

    return {
      migrated,
      isDone: page.isDone,
      cursor: page.continueCursor,
    };
  },
});

/** Run the lowercase gitRemote migration. Pages through both tables. */
export const lowercaseGitRemotes = internalAction({
  args: {},
  handler: async (ctx): Promise<{ projectConsent: number; sessions: number }> => {
    let totalConsent = 0;
    let totalSessions = 0;

    type BatchResult = { migrated: number; isDone: boolean; cursor: string };

    let cursor: string | undefined;
    while (true) {
      const result: BatchResult = await ctx.runMutation(
        internal.migrations.lowercaseProjectConsentBatch,
        { cursor },
      );
      totalConsent += result.migrated;
      if (result.isDone) break;
      cursor = result.cursor;
    }

    cursor = undefined;
    while (true) {
      const result: BatchResult = await ctx.runMutation(
        internal.migrations.lowercaseSessionsBatch,
        { cursor },
      );
      totalSessions += result.migrated;
      if (result.isDone) break;
      cursor = result.cursor;
    }

    return { projectConsent: totalConsent, sessions: totalSessions };
  },
});
