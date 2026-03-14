import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";

/** Append a new global data sharing consent row (never upserts). */
export const submitConsent = mutation({
  args: {
    consent: v.union(
      v.object({ sessionSharing: v.literal(false) }),
      v.object({
        sessionSharing: v.literal(true),
        communityFeatures: v.boolean(),
        publicationExcerpts: v.boolean(),
        creditByName: v.boolean(),
      }),
    ),
  },
  handler: async (ctx, { consent }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      throw new ConvexError("User not found");
    }

    if (consent.sessionSharing) {
      await ctx.db.insert("dataSharingConsent", {
        userId: user._id,
        sessionSharing: true,
        communityFeatures: consent.communityFeatures,
        publicationExcerpts: consent.publicationExcerpts,
        creditByName: consent.creditByName,
        consentedAt: Date.now(),
      });
    } else {
      await ctx.db.insert("dataSharingConsent", {
        userId: user._id,
        sessionSharing: false,
        consentedAt: Date.now(),
      });
    }
  },
});

/** Get the latest consent row for the authenticated user, or null. */
export const getLatestConsent = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return null;
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      return null;
    }

    // Index sorts by _creationTime — latest row is last
    return await ctx.db
      .query("dataSharingConsent")
      .withIndex("by_user_id", (q) => q.eq("userId", user._id))
      .order("desc")
      .first();
  },
});

/** Lightweight consent status for CLI. */
export const getConsentStatus = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return null;
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      return { hasConsent: false, sessionSharing: false };
    }

    const latest = await ctx.db
      .query("dataSharingConsent")
      .withIndex("by_user_id", (q) => q.eq("userId", user._id))
      .order("desc")
      .first();

    if (!latest) {
      return { hasConsent: false, sessionSharing: false };
    }

    return { hasConsent: true, sessionSharing: latest.sessionSharing };
  },
});

/** Append a project enable event. */
export const enableProject = mutation({
  args: { project: v.string() },
  handler: async (ctx, { project }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      throw new ConvexError("User not found");
    }

    await ctx.db.insert("projectConsent", {
      userId: user._id,
      project,
      sessionSharing: true,
      consentedAt: Date.now(),
    });
  },
});

/** Append a project disable event. */
export const disableProject = mutation({
  args: { project: v.string() },
  handler: async (ctx, { project }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      throw new ConvexError("User not found");
    }

    await ctx.db.insert("projectConsent", {
      userId: user._id,
      project,
      sessionSharing: false,
      consentedAt: Date.now(),
    });
  },
});

/** Get active per-project consents (latest event per project where sessionSharing is true). */
export const getEnabledProjects = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return [];
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      return [];
    }

    const allEvents = await ctx.db
      .query("projectConsent")
      .withIndex("by_user_id", (q) => q.eq("userId", user._id))
      .collect();

    // Group by project, keep latest per project
    const latestByProject = new Map<
      string,
      { project: string; sessionSharing: boolean; consentedAt: number }
    >();

    for (const event of allEvents) {
      const existing = latestByProject.get(event.project);
      if (!existing || event.consentedAt > existing.consentedAt) {
        latestByProject.set(event.project, {
          project: event.project,
          sessionSharing: event.sessionSharing,
          consentedAt: event.consentedAt,
        });
      }
    }

    // Return only actively enabled projects
    return [...latestByProject.values()].filter((p) => p.sessionSharing);
  },
});

/** Get distinct projects and session counts for the authenticated user (for existing data step). */
export const getUserSessionProjects = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return [];
    }

    const sessions = await ctx.db
      .query("sessions")
      .withIndex("by_user_id", (q) => q.eq("userId", identity.subject))
      .collect();

    const projectCounts = new Map<string, number>();
    for (const session of sessions) {
      projectCounts.set(
        session.project,
        (projectCounts.get(session.project) ?? 0) + 1,
      );
    }

    return [...projectCounts.entries()]
      .map(([project, count]) => ({ project, count }))
      .sort((a, b) => b.count - a.count);
  },
});

/** Get the list of users with data access (for the consent page access list). */
export const getAccessList = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return [];
    }

    const users = await ctx.db.query("users").collect();

    return users
      .filter((u) => u.hasDataAccess)
      .map((u) => ({
        name: [u.firstName, u.lastName].filter(Boolean).join(" ") || null,
        email: u.email,
      }));
  },
});
