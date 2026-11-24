// Convex tables. dataSharingConsent and projectConsent are append-only event logs: consent
// windows are computed from their full _creationTime history, so rows are inserted, never patched.
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
    // Parent sessions only, kept by refreshChildCounts so the list needn't read children: how many
    // agent sessions it has (capped at MAX_COUNTED_CHILDREN), and how many distinct workflow runs
    // they belong to. Unset means not counted yet, until backfillChildCounts reaches the row.
    agentSessionCount: v.optional(v.number()),
    workflowRunCount: v.optional(v.number()),
    upload: v.optional(
      v.object({
        storageId: v.id("_storage"),
        uploadedAt: v.number(),
      }),
    ),
  })
    .index("by_session_id", ["sessionId"])
    .index("by_user_doc_id", ["userDocId"])
    // A user's parent sessions without scanning past their agents (parentSessionId: undefined), and
    // one parent's children without reading rows other users pinned to its id.
    .index("by_user_and_parent", ["userDocId", "parentSessionId"])
    .index("by_parent_session_id", ["parentSessionId"])
    // First-claim enforcement: a storage blob may only ever be linked to one row (see
    // assertStorageIdUnclaimed in lib/blobs.ts).
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
    // assertStorageIdUnclaimed in lib/blobs.ts).
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

  // Anonymous reading analytics for public articles (/agent-advice). One row per event; `visit` is
  // a random id generated per page load and kept in memory, so it ties one visit's events together
  // and nothing else. No IP, user agent, cookie or full referrer is stored. Written only by the web
  // server's /api/article-events route (convex/articleEvents.ts).
  articleEvents: defineTable({
    day: v.string(), // UTC "YYYY-MM-DD"
    article: v.string(),
    visit: v.string(),
    type: v.union(
      v.literal("view"),
      v.literal("section"),
      v.literal("engaged"),
      v.literal("click"),
      v.literal("toggle"),
    ),
    name: v.optional(v.string()), // section id, or click/toggle name
    venue: v.optional(v.string()),
    referrer: v.optional(v.string()), // host only
    viewport: v.optional(v.string()),
    hash: v.optional(v.string()),
    bucket: v.optional(v.string()), // engaged-time bucket
    deepest: v.optional(v.string()), // deepest section reached, on `engaged`
  }).index("by_day", ["day"]),

  // Fetches of /install.sh and /uninstall.sh, counted by the web server's script routes.
  scriptFetches: defineTable({
    day: v.string(),
    script: v.union(v.literal("install"), v.literal("uninstall")),
    venue: v.optional(v.string()),
  }).index("by_day", ["day"]),

  // GitHub repo traffic, saved daily past GitHub's 14-day window (convex/githubTraffic.ts).
  githubTrafficDaily: defineTable({
    kind: v.union(v.literal("views"), v.literal("clones")),
    day: v.string(), // UTC "YYYY-MM-DD"
    count: v.number(),
    uniques: v.number(),
  }).index("by_kind_day", ["kind", "day"]),

  githubTrafficSnapshots: defineTable({
    kind: v.union(v.literal("referrers"), v.literal("paths")),
    fetchedDay: v.string(),
    items: v.array(
      v.object({
        name: v.string(),
        title: v.optional(v.string()),
        count: v.number(),
        uniques: v.number(),
      }),
    ),
  }).index("by_kind_day", ["kind", "fetchedDay"]),

  linkedRepos: defineTable({
    installationId: v.number(),
    gitRemote: v.string(), // "github.com/owner/repo"
    repoId: v.number(),
  })
    .index("by_git_remote", ["gitRemote"])
    .index("by_installation_id", ["installationId"])
    .index("by_repo_id", ["repoId"]),
});
