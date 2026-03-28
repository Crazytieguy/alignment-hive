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

    // Generate upload URLs for all sessions
    const allSessionIds = [args.sessionId, ...args.agentSessionIds];
    const urls: Record<string, string> = {};
    for (const sid of allSessionIds) {
      urls[sid] = await ctx.storage.generateUploadUrl();
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
        sessionStartGitCommitHash: upload.parentSessionId ? undefined : args.sessionStartGitCommitHash,
      });

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
