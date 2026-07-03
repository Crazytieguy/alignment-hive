import { ConvexError, v } from "convex/values";
import { mutation } from "./_generated/server";
import type { MutationCtx } from "./_generated/server";
import type { Id } from "./_generated/dataModel";
import {
  computeConsentWindows,
  isInConsentWindow,
  extractIdentifiers,
  type ProjectIdentifiers,
} from "@alignment-hive/session-data";
import { upsertUser } from "./lib/users";
import { getMatchingConsentGroup } from "./lib/projectConsent";

/**
 * Verify that the user has active global + project consent for sharing.
 * If lastModified is provided, also checks that it falls within consent windows
 * (defense-in-depth against uploading sessions from revocation gaps).
 */
async function verifyConsent(
  ctx: MutationCtx,
  userId: Id<"users">,
  identifiers: ProjectIdentifiers,
  lastModified?: number,
): Promise<void> {
  // Load all global consent events (single query covers both current-state
  // and consent-window checks, avoiding a redundant .first() query)
  const allGlobalEvents = await ctx.db
    .query("dataSharingConsent")
    .withIndex("by_user_id", (q) => q.eq("userId", userId))
    .collect();

  if (allGlobalEvents.length === 0) {
    throw new ConvexError(
      "Session sharing not enabled — complete consent at https://alignment-hive.com/consent",
    );
  }

  // Latest event is last by timestamp (default sort order)
  const latestConsent = allGlobalEvents[allGlobalEvents.length - 1];
  if (!latestConsent.sessionSharing) {
    throw new ConvexError(
      "Session sharing is disabled — update preferences at https://alignment-hive.com/consent",
    );
  }

  // Find matching project consent group
  const displayName = identifiers.gitRemote ?? identifiers.directory ?? "unknown";
  const group = await getMatchingConsentGroup(ctx, userId, identifiers);

  if (!group || group.events.length === 0) {
    throw new ConvexError(
      `Session sharing not enabled for project "${displayName}" — run /hive:align to enable`,
    );
  }

  const latestEvent = group.events.reduce((a, b) =>
    a.timestamp > b.timestamp ? a : b,
  );
  if (!latestEvent.sessionSharing) {
    throw new ConvexError(
      `Session sharing disabled for project "${displayName}" — run /hive:align to re-enable`,
    );
  }

  // If lastModified is provided, verify it falls within consent windows
  if (lastModified !== undefined) {
    const globalWindows = computeConsentWindows(
      allGlobalEvents.map((e) => ({
        sessionSharing: e.sessionSharing,
        timestamp: e._creationTime,
      })),
    );
    if (!isInConsentWindow(lastModified, globalWindows)) {
      throw new ConvexError(
        "Session was last modified outside an active consent window",
      );
    }

    const projectWindows = computeConsentWindows(group.events);
    if (!isInConsentWindow(lastModified, projectWindows)) {
      throw new ConvexError(
        `Session was last modified outside a consent window for project "${displayName}"`,
      );
    }
  }
}

function isSessionOwner(
  session: { userDocId: Id<"users"> },
  userDocId: Id<"users">,
): boolean {
  return session.userDocId === userDocId;
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
 */
async function assertStorageIdUnclaimed(
  ctx: MutationCtx,
  storageId: Id<"_storage">,
  self: { sessionDocId?: Id<"sessions">; runDocId?: Id<"workflowRuns"> },
): Promise<void> {
  const sessionClaim = await ctx.db
    .query("sessions")
    .withIndex("by_storage_id", (q) => q.eq("upload.storageId", storageId))
    .first();
  if (sessionClaim && sessionClaim._id !== self.sessionDocId) {
    throw new ConvexError("Storage blob is already linked to another record");
  }
  const runClaim = await ctx.db
    .query("workflowRuns")
    .withIndex("by_storage_id", (q) => q.eq("upload.storageId", storageId))
    .first();
  if (runClaim && runClaim._id !== self.runDocId) {
    throw new ConvexError("Storage blob is already linked to another record");
  }
}

/** Upsert session record — creates if new, updates lineCount/lastHeartbeat if existing. Returns the document ID. */
async function upsertSession(
  ctx: MutationCtx,
  userDocId: Id<"users">,
  args: {
    sessionId: string;
    checkoutId: string;
    project?: string;
    directory?: string;
    gitRemote?: string;
    lineCount: number;
    lastModified?: number;
    parentSessionId?: string;
    agentType?: string;
    workflowRunId?: string;
    sessionStartGitCommitHash?: string;
  },
): Promise<Id<"sessions">> {
  const now = Date.now();
  const existing = await ctx.db
    .query("sessions")
    .withIndex("by_session_id", (q) => q.eq("sessionId", args.sessionId))
    .first();

  if (existing) {
    if (!isSessionOwner(existing, userDocId)) {
      throw new ConvexError("Session belongs to different user");
    }
    await ctx.db.patch(existing._id, {
      userDocId,
      lineCount: args.lineCount,
      lastHeartbeat: now,
      ...(args.lastModified !== undefined && {
        lastModified: args.lastModified,
      }),
      // Enrich existing sessions with identifiers if provided
      ...(args.directory && { directory: args.directory }),
      ...(args.gitRemote && { gitRemote: args.gitRemote }),
      ...(args.parentSessionId && { parentSessionId: args.parentSessionId }),
      ...(args.agentType && { agentType: args.agentType }),
      ...(args.workflowRunId && { workflowRunId: args.workflowRunId }),
      ...(args.sessionStartGitCommitHash && {
        sessionStartGitCommitHash: args.sessionStartGitCommitHash,
      }),
    });
    return existing._id;
  } else {
    return await ctx.db.insert("sessions", {
      sessionId: args.sessionId,
      userDocId,
      checkoutId: args.checkoutId,
      project: args.project,
      directory: args.directory,
      gitRemote: args.gitRemote,
      lineCount: args.lineCount,
      lastHeartbeat: now,
      lastModified: args.lastModified,
      parentSessionId: args.parentSessionId,
      agentType: args.agentType,
      workflowRunId: args.workflowRunId,
      sessionStartGitCommitHash: args.sessionStartGitCommitHash,
    });
  }
}

export const heartbeatSession = mutation({
  args: {
    sessionId: v.string(),
    checkoutId: v.string(),
    project: v.optional(v.string()),
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    lineCount: v.number(),
    lastModified: v.optional(v.number()),
    parentSessionId: v.optional(v.string()),
    sessionStartGitCommitHash: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const userDocId = await upsertUser(ctx, identity);
    const identifiers = extractIdentifiers(args);
    await verifyConsent(ctx, userDocId, identifiers, args.lastModified);

    await upsertSession(ctx, userDocId, args);
  },
});


/**
 * Generate upload URLs for a parent session and its agents in one round trip.
 * Only verifies consent and generates URLs — no session record mutations.
 * Session records are created/updated by saveUploads after files are uploaded.
 */
export const generateUploadUrls = mutation({
  args: {
    sessionId: v.string(),
    agentSessionIds: v.array(v.string()),
    // Workflow run ids (wf_<id>) whose sanitized run-metadata blobs need an upload URL.
    workflowRunIds: v.optional(v.array(v.string())),
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    lastModified: v.optional(v.number()),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const userDocId = await upsertUser(ctx, identity);
    const identifiers = extractIdentifiers(args);
    await verifyConsent(ctx, userDocId, identifiers, args.lastModified);

    // Generate upload URLs for the parent, its agents, and its workflow-run blobs.
    // sessionIds (uuid / agent-<id>) and workflowRunIds (wf_<id>) live in disjoint namespaces,
    // so a single keyed map is unambiguous.
    const keys = [args.sessionId, ...args.agentSessionIds, ...(args.workflowRunIds ?? [])];
    const urls: Record<string, string> = {};
    for (const key of keys) {
      urls[key] = await ctx.storage.generateUploadUrl();
    }
    return urls;
  },
});

/**
 * Save uploads for a parent session and its agents atomically.
 * Upserts all session records with full data and links storage blobs.
 * This is the single point of session record mutation for the upload flow.
 * Consent is verified once for the parent — agent sessions inherit parent consent.
 */
export const saveUploads = mutation({
  args: {
    parentSessionId: v.string(),
    // Session metadata — used to upsert the parent session record
    checkoutId: v.string(),
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    lastModified: v.optional(v.number()),
    sessionStartGitCommitHash: v.optional(v.string()),
    // Upload data for parent + agents
    uploads: v.array(v.object({
      sessionId: v.string(),
      storageId: v.id("_storage"),
      summary: v.optional(v.string()),
      lineCount: v.number(),
      parentSessionId: v.optional(v.string()),
      agentType: v.optional(v.string()),
      workflowRunId: v.optional(v.string()),
    })),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const userDocId = await upsertUser(ctx, identity);
    const identifiers = extractIdentifiers(args);
    await verifyConsent(ctx, userDocId, identifiers, args.lastModified);

    // Verify parent session ownership if it already exists
    const existingParent = await ctx.db
      .query("sessions")
      .withIndex("by_session_id", (q) => q.eq("sessionId", args.parentSessionId))
      .first();
    if (existingParent && !isSessionOwner(existingParent, userDocId)) {
      throw new ConvexError("Parent session belongs to different user");
    }

    const now = Date.now();

    for (const upload of args.uploads) {
      // Each upload must be either the parent itself or an agent of this parent
      const isParent = upload.sessionId === args.parentSessionId && !upload.parentSessionId;
      const isAgent = upload.parentSessionId === args.parentSessionId;
      if (!isParent && !isAgent) {
        throw new ConvexError(`Session ${upload.sessionId} is not the parent or an agent of ${args.parentSessionId}`);
      }

      // Upsert the session record with full data + link storage blob
      const sessionDocId = await upsertSession(ctx, userDocId, {
        sessionId: upload.sessionId,
        checkoutId: args.checkoutId,
        directory: args.directory,
        gitRemote: args.gitRemote,
        lineCount: upload.lineCount,
        lastModified: args.lastModified,
        parentSessionId: upload.parentSessionId,
        agentType: upload.agentType,
        workflowRunId: upload.workflowRunId,
        sessionStartGitCommitHash: upload.parentSessionId ? undefined : args.sessionStartGitCommitHash,
      });

      await assertStorageIdUnclaimed(ctx, upload.storageId, { sessionDocId });
      await ctx.db.patch(sessionDocId, {
        ...(upload.summary !== undefined && { summary: upload.summary }),
        upload: {
          storageId: upload.storageId,
          uploadedAt: now,
        },
      });
    }
  },
});

/**
 * Save Workflow run-metadata records for a parent session. The full sanitized run JSON has
 * already been uploaded to storage; this links each blob and stores the indexed scalars.
 * Consent is verified once for the parent — runs inherit the parent's consent/visibility,
 * exactly like agent sessions.
 */
export const saveWorkflowRuns = mutation({
  args: {
    parentSessionId: v.string(),
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    lastModified: v.optional(v.number()),
    runs: v.array(v.object({
      workflowRunId: v.string(),
      runId: v.string(),
      storageId: v.id("_storage"),
      workflowName: v.optional(v.string()),
      summary: v.optional(v.string()),
      status: v.optional(v.string()),
      totalTokens: v.optional(v.number()),
      totalToolCalls: v.optional(v.number()),
      agentCount: v.optional(v.number()),
      durationMs: v.optional(v.number()),
    })),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const userDocId = await upsertUser(ctx, identity);
    const identifiers = extractIdentifiers(args);
    await verifyConsent(ctx, userDocId, identifiers, args.lastModified);

    // Runs inherit the parent's consent/visibility, so the parent session must already exist
    // AND be owned by the caller. Fail closed when it's missing — never pin a run to a
    // parentSessionId the caller doesn't own (the read path also owner-filters as defense in depth).
    const existingParent = await ctx.db
      .query("sessions")
      .withIndex("by_session_id", (q) => q.eq("sessionId", args.parentSessionId))
      .first();
    if (!existingParent || !isSessionOwner(existingParent, userDocId)) {
      throw new ConvexError("Parent session not found or belongs to a different user");
    }

    const now = Date.now();
    for (const run of args.runs) {
      const candidates = await ctx.db
        .query("workflowRuns")
        .withIndex("by_workflow_run_id", (q) => q.eq("workflowRunId", run.workflowRunId))
        .collect();
      const existing = candidates.find(
        (r) => r.userDocId === userDocId && r.parentSessionId === args.parentSessionId,
      );

      await assertStorageIdUnclaimed(ctx, run.storageId, { runDocId: existing?._id });

      // Conditional spreads (=== undefined, not falsy) so a sparser re-upload preserves
      // previously-stored fields instead of clobbering them, and valid 0 values are kept.
      const doc = {
        workflowRunId: run.workflowRunId,
        runId: run.runId,
        parentSessionId: args.parentSessionId,
        userDocId,
        upload: { storageId: run.storageId, uploadedAt: now },
        ...(args.directory !== undefined && { directory: args.directory }),
        ...(args.gitRemote !== undefined && { gitRemote: args.gitRemote }),
        ...(args.lastModified !== undefined && { lastModified: args.lastModified }),
        ...(run.workflowName !== undefined && { workflowName: run.workflowName }),
        ...(run.summary !== undefined && { summary: run.summary }),
        ...(run.status !== undefined && { status: run.status }),
        ...(run.totalTokens !== undefined && { totalTokens: run.totalTokens }),
        ...(run.totalToolCalls !== undefined && { totalToolCalls: run.totalToolCalls }),
        ...(run.agentCount !== undefined && { agentCount: run.agentCount }),
        ...(run.durationMs !== undefined && { durationMs: run.durationMs }),
      };

      if (existing) {
        await ctx.db.patch(existing._id, doc);
      } else {
        await ctx.db.insert("workflowRuns", doc);
      }
    }
  },
});

export const upsertCheckout = mutation({
  args: { checkoutId: v.string() },
  handler: async (ctx, { checkoutId }) => {
    const now = Date.now();

    const existing = await ctx.db
      .query("checkouts")
      .withIndex("by_checkout_id", (q) => q.eq("checkoutId", checkoutId))
      .first();

    if (existing) {
      await ctx.db.patch(existing._id, { lastSeenAt: now });
    } else {
      await ctx.db.insert("checkouts", {
        checkoutId,
        firstSeenAt: now,
        lastSeenAt: now,
      });
    }
  },
});
