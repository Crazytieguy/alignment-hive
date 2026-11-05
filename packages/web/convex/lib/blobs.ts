// Storage blob lifecycle. Rows link blobs through upload.storageId in BLOB_LINK_TABLES, each indexed
// by_storage_id. A new table that links blobs must join that list, or delete-on-replace and
// cleanupOrphanedBlobs (maintenance.ts) would delete blobs it still needs.
import { ConvexError } from "convex/values";
import type { MutationCtx } from "../_generated/server";
import type { Id } from "../_generated/dataModel";

const BLOB_LINK_TABLES = ["sessions", "workflowRuns"] as const;

function findBlobLink(
  ctx: MutationCtx,
  table: (typeof BLOB_LINK_TABLES)[number],
  storageId: Id<"_storage">,
) {
  return ctx.db
    .query(table)
    .withIndex("by_storage_id", (q) => q.eq("upload.storageId", storageId))
    .first();
}

/**
 * First-claim enforcement for storage blobs: a storageId may only ever be linked to a single
 * row. The save mutations take storageIds from the client without any way to verify who
 * uploaded the blob — accepting an already-claimed id would let a caller who somehow learned
 * another user's storageId link it to their own consented row and read the victim's blob
 * through their own signed-URL read path. Every legitimate upload mints a fresh blob
 * (re-uploads included), so a claimed id can never come from this caller's upload. The self
 * params exempt the row being upserted, keeping retries of the same save idempotent.
 * Defense-in-depth companion rule: no query may ever return a raw storageId (see CLAUDE.md).
 *
 * A deleted blob is refused too. A replaced blob has no row claiming it, so without this a late
 * retry of an older save would repoint the row at nothing and delete its current blob.
 */
export async function assertStorageIdUnclaimed(
  ctx: MutationCtx,
  storageId: Id<"_storage">,
  self: { sessionDocId?: Id<"sessions">; runDocId?: Id<"workflowRuns"> },
): Promise<void> {
  const selfIds: Record<(typeof BLOB_LINK_TABLES)[number], string | undefined> = {
    sessions: self.sessionDocId,
    workflowRuns: self.runDocId,
  };
  for (const table of BLOB_LINK_TABLES) {
    const claim = await findBlobLink(ctx, table, storageId);
    if (claim && claim._id !== selfIds[table]) {
      throw new ConvexError("Storage blob is already linked to another record");
    }
  }
  if (!(await ctx.db.system.get("_storage", storageId))) {
    throw new ConvexError("Uploaded file no longer exists — retry the upload");
  }
}

/** Whether any row links this blob. Inside a mutation this sees the mutation's own writes. */
export async function isBlobLinked(
  ctx: MutationCtx,
  storageId: Id<"_storage">,
): Promise<boolean> {
  for (const table of BLOB_LINK_TABLES) {
    if (await findBlobLink(ctx, table, storageId)) return true;
  }
  return false;
}

/**
 * Delete the blob a row just stopped pointing at; call it after the patch. First-claim should mean
 * no other row links it, but rows older than that rule may share a blob, so check rather than
 * assume. Convex serializes this against a concurrent save linking the blob: the index read
 * conflicts with its write, and the retried save is refused because the blob is gone.
 */
export async function deleteReplacedBlob(
  ctx: MutationCtx,
  previous: Id<"_storage"> | undefined,
  current: Id<"_storage">,
): Promise<void> {
  if (!previous || previous === current) return;
  if (await isBlobLinked(ctx, previous)) return;
  // A row can already dangle (a blob deleted by hand); that must not fail the save replacing it.
  if (await ctx.db.system.get("_storage", previous)) {
    await ctx.storage.delete(previous);
  }
}
