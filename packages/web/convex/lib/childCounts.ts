import type { MutationCtx } from "../_generated/server";

// Counting stops here, so a badge can never push a save or the backfill over Convex's read limits
// (32,000 documents, 16 MiB): at most 1,000 summary characters plus the rest of a row, that's under
// 16 MiB even if every character is 4 bytes. A workflow session is about 200 agents.
export const MAX_COUNTED_CHILDREN = 3000;

/**
 * Recount a parent's denormalized child counts, which is what the session list shows instead of
 * reading every child per row. Called by any write that changes a parent's child set, and by
 * backfillChildCounts (maintenance.ts). Only the owner's children count, so a row pinned to
 * someone else's parentSessionId can't write to their row. Writes 0 too, because an unset count
 * means "not counted yet". Returns whether the parent changed.
 */
export async function refreshChildCounts(
  ctx: MutationCtx,
  parentSessionId: string,
): Promise<boolean> {
  const parent = await ctx.db
    .query("sessions")
    .withIndex("by_session_id", (q) => q.eq("sessionId", parentSessionId))
    .first();
  if (!parent) return false;

  const children = await ctx.db
    .query("sessions")
    .withIndex("by_parent_session_id", (q) =>
      q.eq("parentSessionId", parentSessionId),
    )
    .take(MAX_COUNTED_CHILDREN);
  const owned = children.filter((c) => c.userDocId === parent.userDocId);
  const agentSessionCount = owned.length;
  const workflowRunCount = new Set(owned.flatMap((c) => c.workflowRunId ?? []))
    .size;

  if (
    parent.agentSessionCount === agentSessionCount &&
    parent.workflowRunCount === workflowRunCount
  ) {
    return false;
  }
  await ctx.db.patch(parent._id, { agentSessionCount, workflowRunCount });
  return true;
}
