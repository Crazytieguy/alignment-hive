import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import type { MutationCtx } from "./_generated/server";
import type { UserIdentity } from "convex/server";
import type { Id } from "./_generated/dataModel";

/** Verify that the user has active global + project consent for sharing. */
async function verifyConsent(
  ctx: MutationCtx,
  userId: Id<"users">,
  project: string,
): Promise<void> {
  // Check global consent (latest by _creationTime via desc order)
  const latestConsent = await ctx.db
    .query("dataSharingConsent")
    .withIndex("by_user_id", (q) => q.eq("userId", userId))
    .order("desc")
    .first();

  if (!latestConsent) {
    throw new ConvexError(
      "Session sharing not enabled — complete consent at https://alignment-hive.com/consent",
    );
  }

  if (!latestConsent.sessionSharing) {
    throw new ConvexError(
      "Session sharing is disabled — update preferences at https://alignment-hive.com/consent",
    );
  }

  // Check project consent (latest by _creationTime via desc order)
  const latestProject = await ctx.db
    .query("projectConsent")
    .withIndex("by_user_project", (q) =>
      q.eq("userId", userId).eq("project", project),
    )
    .order("desc")
    .first();

  if (!latestProject) {
    throw new ConvexError(
      `Session sharing not enabled for project "${project}" — run /hive:align to enable`,
    );
  }

  if (!latestProject.sessionSharing) {
    throw new ConvexError(
      `Session sharing disabled for project "${project}" — run /hive:align to re-enable`,
    );
  }
}

/** Upsert user record with latest identity info from JWT */
async function upsertUser(
  ctx: MutationCtx,
  identity: UserIdentity,
): Promise<void> {
  const userId = identity.subject;
  const givenName = (identity as Record<string, unknown>)["given_name"] as
    | string
    | undefined;
  const familyName = (identity as Record<string, unknown>)["family_name"] as
    | string
    | undefined;

  const existingUser = await ctx.db
    .query("users")
    .withIndex("by_workos_id", (q) => q.eq("workosId", userId))
    .first();

  if (existingUser) {
    if (
      existingUser.firstName !== givenName ||
      existingUser.lastName !== familyName ||
      existingUser.email !== identity.email
    ) {
      await ctx.db.patch(existingUser._id, {
        email: identity.email ?? existingUser.email,
        firstName: givenName ?? existingUser.firstName,
        lastName: familyName ?? existingUser.lastName,
      });
    }
  } else if (identity.email) {
    await ctx.db.insert("users", {
      workosId: userId,
      email: identity.email,
      firstName: givenName,
      lastName: familyName,
    });
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
    });
  } else {
    await ctx.db.insert("sessions", {
      sessionId: args.sessionId,
      userId,
      checkoutId: args.checkoutId,
      project: args.project,
      lineCount: args.lineCount,
      lastHeartbeat: now,
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
    parentSessionId: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    await upsertUser(ctx, identity);

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();
    if (!user) {
      throw new ConvexError("User record not found — complete signup at alignment-hive.com");
    }
    await verifyConsent(ctx, user._id, args.project);

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
      await upsertUser(ctx, identity);

      const user = await ctx.db
        .query("users")
        .withIndex("by_workos_id", (q) => q.eq("workosId", userId))
        .first();
      if (!user) {
        throw new ConvexError("User record not found");
      }
      await verifyConsent(ctx, user._id, args.project);

      await upsertSession(ctx, userId, {
        sessionId: args.sessionId,
        checkoutId: args.checkoutId,
        project: args.project,
        lineCount: args.lineCount,
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

      // Verify consent using existing session's project
      const user = await ctx.db
        .query("users")
        .withIndex("by_workos_id", (q) => q.eq("workosId", userId))
        .first();
      if (!user) {
        throw new ConvexError("User record not found");
      }
      await verifyConsent(ctx, user._id, session.project);
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

    // Verify consent using the session's project
    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();
    if (!user) {
      throw new ConvexError("User record not found");
    }
    await verifyConsent(ctx, user._id, session.project);

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
