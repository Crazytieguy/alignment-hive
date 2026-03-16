import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import type { MutationCtx } from "./_generated/server";
import type { Id } from "./_generated/dataModel";
import {
  computeConsentWindows,
  isInConsentWindow,
} from "@alignment-hive/session-data";
import { upsertUser } from "./lib/users";

/**
 * Verify that the user has active global + project consent for sharing.
 * If lastModified is provided, also checks that it falls within consent windows
 * (defense-in-depth against uploading sessions from revocation gaps).
 */
async function verifyConsent(
  ctx: MutationCtx,
  userId: Id<"users">,
  project: string,
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

  // Latest event is last by _creationTime (default sort order)
  const latestConsent = allGlobalEvents[allGlobalEvents.length - 1];
  if (!latestConsent.sessionSharing) {
    throw new ConvexError(
      "Session sharing is disabled — update preferences at https://alignment-hive.com/consent",
    );
  }

  // Load all project consent events
  const allProjectEvents = await ctx.db
    .query("projectConsent")
    .withIndex("by_user_project", (q) =>
      q.eq("userId", userId).eq("project", project),
    )
    .collect();

  if (allProjectEvents.length === 0) {
    throw new ConvexError(
      `Session sharing not enabled for project "${project}" — run /hive:align to enable`,
    );
  }

  const latestProject = allProjectEvents[allProjectEvents.length - 1];
  if (!latestProject.sessionSharing) {
    throw new ConvexError(
      `Session sharing disabled for project "${project}" — run /hive:align to re-enable`,
    );
  }

  // If lastModified is provided, verify it falls within consent windows
  if (lastModified !== undefined) {
    const globalWindows = computeConsentWindows(allGlobalEvents);
    if (!isInConsentWindow(lastModified, globalWindows)) {
      throw new ConvexError(
        "Session was last modified outside an active consent window",
      );
    }

    const projectWindows = computeConsentWindows(allProjectEvents);
    if (!isInConsentWindow(lastModified, projectWindows)) {
      throw new ConvexError(
        `Session was last modified outside a consent window for project "${project}"`,
      );
    }
  }
}

/** Upsert session record — creates if new, updates lineCount/lastHeartbeat if existing */
async function upsertSession(
  ctx: MutationCtx,
  userId: string,
  args: {
    sessionId: string;
    checkoutId: string;
    project: string;
    lineCount: number;
    lastModified?: number;
    parentSessionId?: string;
  },
): Promise<void> {
  const now = Date.now();
  const existing = await ctx.db
    .query("sessions")
    .withIndex("by_session_id", (q) => q.eq("sessionId", args.sessionId))
    .first();

  if (existing) {
    if (existing.userId !== userId) {
      throw new ConvexError("Session belongs to different user");
    }
    await ctx.db.patch(existing._id, {
      lineCount: args.lineCount,
      lastHeartbeat: now,
      ...(args.lastModified !== undefined && {
        lastModified: args.lastModified,
      }),
    });
  } else {
    await ctx.db.insert("sessions", {
      sessionId: args.sessionId,
      userId,
      checkoutId: args.checkoutId,
      project: args.project,
      lineCount: args.lineCount,
      lastHeartbeat: now,
      lastModified: args.lastModified,
      parentSessionId: args.parentSessionId,
    });
  }
}

export const heartbeatSession = mutation({
  args: {
    sessionId: v.string(),
    checkoutId: v.string(),
    project: v.string(),
    lineCount: v.number(),
    lastModified: v.optional(v.number()),
    parentSessionId: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const userId = await upsertUser(ctx, identity);
    await verifyConsent(ctx, userId, args.project, args.lastModified);

    await upsertSession(ctx, identity.subject, args);
  },
});

export const generateUploadUrl = mutation({
  args: {
    sessionId: v.string(),
    // Optional heartbeat fields — if provided, upserts the session in the same round trip
    checkoutId: v.optional(v.string()),
    project: v.optional(v.string()),
    lineCount: v.optional(v.number()),
    lastModified: v.optional(v.number()),
    parentSessionId: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const userId = identity.subject;

    if (args.checkoutId && args.project && args.lineCount !== undefined) {
      // Inline heartbeat: upsert user + session in the same round trip
      const userDocId = await upsertUser(ctx, identity);
      await verifyConsent(
        ctx,
        userDocId,
        args.project,
        args.lastModified,
      );

      await upsertSession(ctx, userId, {
        sessionId: args.sessionId,
        checkoutId: args.checkoutId,
        project: args.project,
        lineCount: args.lineCount,
        lastModified: args.lastModified,
        parentSessionId: args.parentSessionId,
      });
    } else {
      // No heartbeat fields — just verify session exists (backwards compat)
      const session = await ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) => q.eq("sessionId", args.sessionId))
        .first();

      if (!session) {
        throw new ConvexError("Session not found - heartbeat first");
      }
      if (session.userId !== userId) {
        throw new ConvexError("Session belongs to different user");
      }

      // Verify consent using existing session's project and lastModified
      const userDocId = await upsertUser(ctx, identity);
      await verifyConsent(
        ctx,
        userDocId,
        session.project,
        args.lastModified ?? session.lastModified,
      );
    }

    return await ctx.storage.generateUploadUrl();
  },
});

export const saveUpload = mutation({
  args: {
    sessionId: v.string(),
    storageId: v.id("_storage"),
    summary: v.optional(v.string()),
  },
  handler: async (ctx, { sessionId, storageId, summary }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const session = await ctx.db
      .query("sessions")
      .withIndex("by_session_id", (q) => q.eq("sessionId", sessionId))
      .first();

    if (!session) {
      throw new ConvexError("Session not found");
    }
    if (session.userId !== identity.subject) {
      throw new ConvexError("Session belongs to different user");
    }

    // Verify consent using the session's project and lastModified
    const userDocId = await upsertUser(ctx, identity);
    await verifyConsent(ctx, userDocId, session.project, session.lastModified);

    await ctx.db.patch(session._id, {
      ...(summary && { summary }),
      upload: {
        storageId,
        uploadedAt: Date.now(),
      },
    });
  },
});

export const listUserSessions = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return [];
    }

    return await ctx.db
      .query("sessions")
      .withIndex("by_user_id", (q) => q.eq("userId", identity.subject))
      .collect();
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
