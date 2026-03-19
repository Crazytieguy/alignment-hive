import { v } from "convex/values";
import { internalAction, internalMutation } from "./_generated/server";
import { internal } from "./_generated/api";

const BATCH_SIZE = 100;

type BatchResult = { migrated: number; isDone: boolean; cursor: string };

// --- Migration: remove legacy fields (project, consentedAt) ---

export const removeLegacyFieldsConsentBatch = internalMutation({
  args: { cursor: v.optional(v.string()) },
  handler: async (ctx, { cursor }) => {
    const page = await ctx.db
      .query("dataSharingConsent")
      .paginate({ numItems: BATCH_SIZE, cursor: cursor ?? null });

    let migrated = 0;
    for (const doc of page.page) {
      if ("consentedAt" in doc) {
        await ctx.db.patch(doc._id, { consentedAt: undefined });
        migrated++;
      }
    }

    return { migrated, isDone: page.isDone, cursor: page.continueCursor };
  },
});

export const removeLegacyFieldsProjectBatch = internalMutation({
  args: { cursor: v.optional(v.string()) },
  handler: async (ctx, { cursor }) => {
    const page = await ctx.db
      .query("projectConsent")
      .paginate({ numItems: BATCH_SIZE, cursor: cursor ?? null });

    let migrated = 0;
    for (const doc of page.page) {
      const patch: Record<string, undefined> = {};
      if ("consentedAt" in doc) patch.consentedAt = undefined;
      if ("project" in doc) patch.project = undefined;
      if (Object.keys(patch).length > 0) {
        await ctx.db.patch(doc._id, patch);
        migrated++;
      }
    }

    return { migrated, isDone: page.isDone, cursor: page.continueCursor };
  },
});

export const removeLegacyFields = internalAction({
  args: {},
  handler: async (ctx) => {
    let totalConsent = 0;
    let totalProject = 0;

    let cursor: string | undefined;
    while (true) {
      const result: BatchResult = await ctx.runMutation(
        internal.migrations.removeLegacyFieldsConsentBatch,
        { cursor },
      );
      totalConsent += result.migrated;
      if (result.isDone) break;
      cursor = result.cursor;
    }

    cursor = undefined;
    while (true) {
      const result: BatchResult = await ctx.runMutation(
        internal.migrations.removeLegacyFieldsProjectBatch,
        { cursor },
      );
      totalProject += result.migrated;
      if (result.isDone) break;
      cursor = result.cursor;
    }

    return { consent: totalConsent, projectConsent: totalProject };
  },
});
