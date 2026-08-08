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
    userDocId: v.id("users"),
    checkoutId: v.string(),
    project: v.optional(v.string()), // legacy — use directory/gitRemote
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    lineCount: v.number(),
    lastHeartbeat: v.number(),
    lastModified: v.optional(v.number()),
    parentSessionId: v.optional(v.string()),
    // Agent metadata (subagent sessions only): the agent's type (e.g. "general-purpose",
    // "Explore", "workflow-subagent") and, for workflow subagents, the wf_<id> run they belong to.
    agentType: v.optional(v.string()),
    workflowRunId: v.optional(v.string()),
    summary: v.optional(v.string()),
    sessionStartGitCommitHash: v.optional(v.string()),
    upload: v.optional(
      v.object({
        storageId: v.id("_storage"),
        uploadedAt: v.number(),
      }),
    ),
  })
    .index("by_session_id", ["sessionId"])
    .index("by_user_doc_id", ["userDocId"])
    .index("by_parent_session_id", ["parentSessionId"])
    // First-claim enforcement: a storage blob may only ever be linked to one row (see
    // assertStorageIdUnclaimed in sessions.ts).
    .index("by_storage_id", ["upload.storageId"]),

  // Run-level metadata for a Workflow-tool run (wf_<id>). The full sanitized run JSON
  // (script/result/logs/etc.) lives in storage (upload.storageId); these are the indexed
  // scalars used to list/group a parent's runs. Inherits the parent session's consent.
  workflowRuns: defineTable({
    workflowRunId: v.string(), // wf_<id> dir name; join key with sessions.workflowRunId
    runId: v.string(),
    parentSessionId: v.string(),
    userDocId: v.id("users"),
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    workflowName: v.optional(v.string()),
    summary: v.optional(v.string()),
    status: v.optional(v.string()),
    totalTokens: v.optional(v.number()),
    totalToolCalls: v.optional(v.number()),
    agentCount: v.optional(v.number()),
    durationMs: v.optional(v.number()),
    lastModified: v.optional(v.number()),
    upload: v.object({
      storageId: v.id("_storage"),
      uploadedAt: v.number(),
    }),
  })
    .index("by_workflow_run_id", ["workflowRunId"])
    .index("by_parent_session_id", ["parentSessionId"])
    .index("by_user_doc_id", ["userDocId"])
    // First-claim enforcement: a storage blob may only ever be linked to one row (see
    // assertStorageIdUnclaimed in sessions.ts).
    .index("by_storage_id", ["upload.storageId"]),

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
      }),
      v.object({
        userId: v.id("users"),
        sessionSharing: v.literal(true),
        communityFeatures: v.boolean(),
        publicationExcerpts: v.boolean(),
        creditByName: v.boolean(),
      }),
    ),
  ).index("by_user_id", ["userId"]),

  // At least one of directory/gitRemote must be set (enforced by v.union variants).
  projectConsent: defineTable(
    v.union(
      // directory required, gitRemote optional
      v.object({
        userId: v.id("users"),
        directory: v.string(),
        gitRemote: v.optional(v.string()),
        sessionSharing: v.boolean(),
      }),
      // gitRemote required, directory optional
      v.object({
        userId: v.id("users"),
        directory: v.optional(v.string()),
        gitRemote: v.string(),
        sessionSharing: v.boolean(),
      }),
    ),
  )
    .index("by_user_id", ["userId"]),

  dataAccessorAgreements: defineTable({
    userId: v.id("users"),
    agreementVersion: v.string(),
  }).index("by_user_id", ["userId"]),

  githubInstallations: defineTable({
    installationId: v.number(),
    accountLogin: v.string(),
    accountId: v.number(),
    updatedAt: v.number(),
  })
    .index("by_installation_id", ["installationId"]),

  // Single-use consulting-feedback tokens, keyed by sha256 of the token id so the raw token is
  // never stored. `pending` marks an in-flight submission claim (reclaimable after a TTL);
  // `redeemed` is final. Responses themselves live in a Google Sheet, never here — keeping the
  // token store and the response store separate is what makes feedback anonymous.
  feedbackTokens: defineTable({
    tokenHash: v.string(),
    status: v.union(v.literal("pending"), v.literal("redeemed")),
    claimedAt: v.number(),
  }).index("by_token_hash", ["tokenHash"]),

  linkedRepos: defineTable({
    installationId: v.number(),
    gitRemote: v.string(), // "github.com/owner/repo"
    repoId: v.number(),
  })
    .index("by_git_remote", ["gitRemote"])
    .index("by_installation_id", ["installationId"])
    .index("by_repo_id", ["repoId"]),
});
