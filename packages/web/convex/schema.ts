import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

export default defineSchema({
  users: defineTable({
    workosId: v.string(),
    email: v.string(),
    firstName: v.optional(v.string()),
    lastName: v.optional(v.string()),
    hasDataAccess: v.optional(v.boolean()),
  })
    .index("by_workos_id", ["workosId"])
    .index("by_email", ["email"]),

  sessions: defineTable({
    sessionId: v.string(),
    userId: v.string(),
    checkoutId: v.string(),
    project: v.string(),
    lineCount: v.number(),
    lastHeartbeat: v.number(),
    parentSessionId: v.optional(v.string()),
    summary: v.optional(v.string()),
    upload: v.optional(
      v.object({
        storageId: v.id("_storage"),
        uploadedAt: v.number(),
      }),
    ),
  })
    .index("by_session_id", ["sessionId"])
    .index("by_user_id", ["userId"])
    .index("by_parent_session_id", ["parentSessionId"]),

  checkouts: defineTable({
    checkoutId: v.string(),
    firstSeenAt: v.number(),
    lastSeenAt: v.number(),
  }).index("by_checkout_id", ["checkoutId"]),

  dataSharingConsent: defineTable(
    v.union(
      v.object({
        userId: v.id("users"),
        sessionSharing: v.literal(false),
        consentedAt: v.number(),
      }),
      v.object({
        userId: v.id("users"),
        sessionSharing: v.literal(true),
        communityFeatures: v.boolean(),
        publicationExcerpts: v.boolean(),
        creditByName: v.boolean(),
        consentedAt: v.number(),
      }),
    ),
  ).index("by_user_id", ["userId"]),

  projectConsent: defineTable(
    v.union(
      v.object({
        userId: v.id("users"),
        project: v.string(),
        sessionSharing: v.literal(false),
        consentedAt: v.number(),
      }),
      v.object({
        userId: v.id("users"),
        project: v.string(),
        sessionSharing: v.literal(true),
        consentedAt: v.number(),
      }),
    ),
  )
    .index("by_user_id", ["userId"])
    .index("by_user_project", ["userId", "project"]),
});
