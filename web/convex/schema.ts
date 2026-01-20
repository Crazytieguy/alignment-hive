import { defineSchema, defineTable } from 'convex/server';
import { v } from 'convex/values';

export default defineSchema({
  sessions: defineTable({
    sessionId: v.string(),
    userId: v.string(),
    checkoutId: v.string(),
    project: v.string(),
    lineCount: v.number(),
    lastHeartbeat: v.number(),
    parentSessionId: v.optional(v.string()),
    upload: v.optional(
      v.object({
        storageId: v.id('_storage'),
        uploadedAt: v.number(),
      })
    ),
  })
    .index('by_session_id', ['sessionId'])
    .index('by_user_id', ['userId'])
    .index('by_parent_session_id', ['parentSessionId']),

  checkouts: defineTable({
    checkoutId: v.string(),
    firstSeenAt: v.number(),
    lastSeenAt: v.number(),
  }).index('by_checkout_id', ['checkoutId']),
});
