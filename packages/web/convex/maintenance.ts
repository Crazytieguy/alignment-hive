// Upkeep run by hand, never on a schedule. Each action drives small internal mutations and returns
// totals, and both are idempotent: a timed-out or interrupted run is simply run again.
//
//   bunx convex run maintenance:backfillChildCounts
//   bunx convex run maintenance:cleanupOrphanedBlobs '{"dryRun": true}'
import { v } from "convex/values";
import { internalAction, internalMutation, internalQuery } from "./_generated/server";
import { internal } from "./_generated/api";
import { isBlobLinked } from "./lib/blobs";
import { refreshChildCounts } from "./lib/childCounts";

const PARENT_PAGE = 20; // parents recounted at once, each in its own mutation
const BLOB_BATCH = 200;

// An upload's blob stays unlinked until its save mutation lands, so a young unlinked blob may be
// an upload in flight. Far longer than any upload: the CLI saves right after sending.
const ORPHAN_MIN_AGE_MS = 24 * 60 * 60 * 1000;

const cursorArg = v.union(v.string(), v.null());

export const parentSessionIdsPage = internalQuery({
  args: { cursor: cursorArg },
  handler: async (ctx, { cursor }) => {
    const { page, continueCursor, isDone } = await ctx.db
      .query("sessions")
      .withIndex("by_parent_session_id", (q) =>
        q.eq("parentSessionId", undefined),
      )
      .paginate({ cursor, numItems: PARENT_PAGE });
    return { sessionIds: page.map((s) => s.sessionId), continueCursor, isDone };
  },
});

export const recountParent = internalMutation({
  args: { parentSessionId: v.string() },
  handler: (ctx, { parentSessionId }) => refreshChildCounts(ctx, parentSessionId),
});

/**
 * Set agentSessionCount / workflowRunCount on every parent session that predates them. A parent
 * that fails is reported in `failed` rather than stopping the run, so it can't block the rest.
 */
export const backfillChildCounts = internalAction({
  args: {},
  handler: async (
    ctx,
  ): Promise<{ parents: number; updated: number; failed: Array<string> }> => {
    const totals = { parents: 0, updated: 0, failed: [] as Array<string> };
    let cursor: string | null = null;
    for (;;) {
      const page: { sessionIds: Array<string>; continueCursor: string; isDone: boolean } =
        await ctx.runQuery(internal.maintenance.parentSessionIdsPage, { cursor });
      const results = await Promise.allSettled(
        page.sessionIds.map((parentSessionId) =>
          ctx.runMutation(internal.maintenance.recountParent, { parentSessionId }),
        ),
      );
      results.forEach((result, i) => {
        if (result.status === "rejected") totals.failed.push(page.sessionIds[i]);
        else if (result.value) totals.updated++;
      });
      totals.parents += page.sessionIds.length;
      if (page.isDone) return totals;
      cursor = page.continueCursor;
    }
  },
});

type BlobTotals = {
  blobs: number;
  linked: number;
  tooYoung: number;
  orphans: number;
  orphanBytes: number;
};

export const cleanupOrphanedBlobsBatch = internalMutation({
  args: { cursor: cursorArg, dryRun: v.boolean() },
  handler: async (ctx, { cursor, dryRun }) => {
    const { page, continueCursor, isDone } = await ctx.db.system
      .query("_storage")
      .paginate({ cursor, numItems: BLOB_BATCH });

    const counts: BlobTotals = { blobs: page.length, linked: 0, tooYoung: 0, orphans: 0, orphanBytes: 0 };
    const minCreationTime = Date.now() - ORPHAN_MIN_AGE_MS;
    for (const blob of page) {
      if (await isBlobLinked(ctx, blob._id)) {
        counts.linked++;
      } else if (blob._creationTime > minCreationTime) {
        counts.tooYoung++;
      } else {
        counts.orphans++;
        counts.orphanBytes += blob.size;
        if (!dryRun) await ctx.storage.delete(blob._id);
      }
    }
    return { counts, continueCursor, isDone };
  },
});

/**
 * Delete blobs no row links and older than ORPHAN_MIN_AGE_MS: those replaced before
 * delete-on-replace, and uploads whose save never landed. With dryRun it only counts them.
 */
export const cleanupOrphanedBlobs = internalAction({
  args: { dryRun: v.boolean() },
  handler: async (ctx, { dryRun }): Promise<BlobTotals> => {
    const totals: BlobTotals = { blobs: 0, linked: 0, tooYoung: 0, orphans: 0, orphanBytes: 0 };
    let cursor: string | null = null;
    for (;;) {
      const batch: { counts: BlobTotals; continueCursor: string; isDone: boolean } =
        await ctx.runMutation(internal.maintenance.cleanupOrphanedBlobsBatch, { cursor, dryRun });
      totals.blobs += batch.counts.blobs;
      totals.linked += batch.counts.linked;
      totals.tooYoung += batch.counts.tooYoung;
      totals.orphans += batch.counts.orphans;
      totals.orphanBytes += batch.counts.orphanBytes;
      if (batch.isDone) return totals;
      cursor = batch.continueCursor;
    }
  },
});
