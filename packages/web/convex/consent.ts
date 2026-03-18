import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import { extractIdentifiers } from "@alignment-hive/session-data";
import { upsertUser } from "./lib/users";
import { loadAndGroupUserConsent, getMatchingConsentGroup } from "./lib/projectConsent";

/** Validator for project identifiers — at least one of directory/gitRemote required. */
const projectIdentifierArgs = v.union(
  v.object({ directory: v.string(), gitRemote: v.optional(v.string()) }),
  v.object({ directory: v.optional(v.string()), gitRemote: v.string() }),
);

/** Validate non-empty strings (defense in depth). */
function validateIdentifiers(ids: { directory?: string; gitRemote?: string }): void {
  if (!ids.directory && !ids.gitRemote) {
    throw new ConvexError("At least one of directory or gitRemote is required");
  }
  if (ids.directory !== undefined && ids.directory.trim() === "") {
    throw new ConvexError("directory must not be empty");
  }
  if (ids.gitRemote !== undefined && ids.gitRemote.trim() === "") {
    throw new ConvexError("gitRemote must not be empty");
  }
}

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

    const userId = await upsertUser(ctx, identity);

    if (consent.sessionSharing) {
      await ctx.db.insert("dataSharingConsent", {
        userId,
        sessionSharing: true,
        communityFeatures: consent.communityFeatures,
        publicationExcerpts: consent.publicationExcerpts,
        creditByName: consent.creditByName,
        consentedAt: Date.now(),
      });
    } else {
      await ctx.db.insert("dataSharingConsent", {
        userId,
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
  args: { identifier: projectIdentifierArgs },
  handler: async (ctx, { identifier }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }
    validateIdentifiers(identifier);

    const userId = await upsertUser(ctx, identity);

    const now = Date.now();
    if (identifier.directory) {
      await ctx.db.insert("projectConsent", {
        userId,
        directory: identifier.directory,
        gitRemote: identifier.gitRemote,
        sessionSharing: true,
        consentedAt: now,
      });
    } else {
      await ctx.db.insert("projectConsent", {
        userId,
        directory: identifier.directory,
        gitRemote: identifier.gitRemote!,
        sessionSharing: true,
        consentedAt: now,
      });
    }
  },
});

/** Append a project disable event. */
export const disableProject = mutation({
  args: { identifier: projectIdentifierArgs },
  handler: async (ctx, { identifier }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new ConvexError("Not authenticated");
    }
    validateIdentifiers(identifier);

    const userId = await upsertUser(ctx, identity);

    const now = Date.now();
    if (identifier.directory) {
      await ctx.db.insert("projectConsent", {
        userId,
        directory: identifier.directory,
        gitRemote: identifier.gitRemote,
        sessionSharing: false,
        consentedAt: now,
      });
    } else {
      await ctx.db.insert("projectConsent", {
        userId,
        directory: identifier.directory,
        gitRemote: identifier.gitRemote!,
        sessionSharing: false,
        consentedAt: now,
      });
    }
  },
});

/** Get active per-project consents (latest event per group where sessionSharing is true). */
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

    const { groups } = await loadAndGroupUserConsent(ctx, user._id);

    return groups
      .map((group) => {
        const latest = group.events.reduce((a, b) =>
          a.consentedAt > b.consentedAt ? a : b,
        );
        return {
          directories: [...group.directories],
          gitRemotes: [...group.gitRemotes],
          sessionSharing: latest.sessionSharing,
          consentedAt: latest.consentedAt,
        };
      })
      .filter((g) => g.sessionSharing);
  },
});

/** Get all per-project consents (latest event per group, including disabled). */
export const getAllProjects = query({
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

    const { groups } = await loadAndGroupUserConsent(ctx, user._id);

    return groups.map((group) => {
      const latest = group.events.reduce((a, b) =>
        a.consentedAt > b.consentedAt ? a : b,
      );
      return {
        directories: [...group.directories],
        gitRemotes: [...group.gitRemotes],
        sessionSharing: latest.sessionSharing,
        consentedAt: latest.consentedAt,
      };
    });
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
      const ids = extractIdentifiers(session);
      const key = ids.gitRemote ?? ids.directory ?? "unknown";
      projectCounts.set(key, (projectCounts.get(key) ?? 0) + 1);
    }

    return [...projectCounts.entries()]
      .map(([project, count]) => ({ project, count }))
      .sort((a, b) => b.count - a.count);
  },
});

/** Get consent event history for the authenticated user (for consent window computation).
 *  Returns global and project-level events so the CLI can check consent windows locally.
 *  Project events are pre-grouped: returns all events for the matching project group. */
export const getConsentHistory = query({
  args: {
    // Accept both new identifiers and legacy project string
    directory: v.optional(v.string()),
    gitRemote: v.optional(v.string()),
    project: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return null;
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      return { global: [], project: [] };
    }

    const globalEvents = await ctx.db
      .query("dataSharingConsent")
      .withIndex("by_user_id", (q) => q.eq("userId", user._id))
      .collect();

    // Resolve identifiers from args (supports both new and legacy)
    const identifiers = extractIdentifiers(args);
    const group = await getMatchingConsentGroup(ctx, user._id, identifiers);

    return {
      global: globalEvents.map((e) => ({
        sessionSharing: e.sessionSharing,
        consentedAt: e.consentedAt,
      })),
      project: (group?.events ?? []).map((e) => ({
        sessionSharing: e.sessionSharing,
        consentedAt: e.consentedAt,
      })),
    };
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
