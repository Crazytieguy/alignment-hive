/**
 * Authorized session data queries. All queries here MUST:
 * 1. Call requireAuthorized() — rejects unauthenticated and unauthorized users
 * 2. Apply buildConsentFilter() for reader role — ensures readers only see
 *    sessions within consent windows
 * 3. Respect child session inheritance — children inherit parent visibility
 *
 * See CLAUDE.md "Session Data Access Control" for the full security model.
 */
import { paginationOptsValidator } from "convex/server";
import { v } from "convex/values";
import {
  isKnownContentBlock,
  parseKnownEntry,
  extractIdentifiers,
  type KnownEntry,
} from "@alignment-hive/session-data";
import type { Id } from "./_generated/dataModel";
import {
  query,
  internalAction,
  internalMutation,
  internalQuery,
} from "./_generated/server";
import { internal } from "./_generated/api";
import { stream } from "convex-helpers/server/stream";
import schema from "./schema";
import { requireAuthorized } from "./lib/auth";
import { buildConsentFilter } from "./lib/consentVisibility";

export const listSessions = query({
  args: {
    paginationOpts: paginationOptsValidator,
    excludeUserIds: v.optional(v.array(v.string())),
    excludeUnknownUsers: v.optional(v.boolean()),
    excludeProjects: v.optional(v.array(v.string())),
    hasUpload: v.optional(v.boolean()),
  },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) {
      return { page: [], isDone: true, continueCursor: "" };
    }

    const allUsers = await ctx.db.query("users").collect();
    const userMap = new Map(allUsers.map((u) => [u.workosId, u]));

    const consentFilter =
      auth.role === "reader"
        ? await buildConsentFilter(ctx, allUsers)
        : null;

    // Build exclude sets for the filter predicate
    const excludeUserIds = new Set(args.excludeUserIds ?? []);
    const excludeProjects = new Set(args.excludeProjects ?? []);
    const knownUserIds = new Set(allUsers.map((u) => u.workosId));

    // For readers, use stream+filterWith for correct pre-pagination filtering.
    // For admins, use standard Convex query+paginate.
    if (consentFilter) {
      const paginatedSessions = await stream(ctx.db, schema)
        .query("sessions")
        .withIndex("by_parent_session_id", (q) =>
          q.eq("parentSessionId", undefined),
        )
        .order("desc")
        .filterWith(async (session) => {
          if (excludeUserIds.has(session.userId)) return false;
          if (args.excludeUnknownUsers && !knownUserIds.has(session.userId))
            return false;
          const ids = extractIdentifiers(session);
          if ((ids.gitRemote && excludeProjects.has(ids.gitRemote)) ||
              (ids.directory && excludeProjects.has(ids.directory))) return false;
          return consentFilter(session);
        })
        .paginate(args.paginationOpts);

      const childCounts = await Promise.all(
        paginatedSessions.page.map(async (session) => {
          const children = await ctx.db
            .query("sessions")
            .withIndex("by_parent_session_id", (q) =>
              q.eq("parentSessionId", session.sessionId),
            )
            .collect();
          return children.length;
        }),
      );

      return {
        ...paginatedSessions,
        page: paginatedSessions.page.map((session, i) => ({
          ...session,
          user: userMap.get(session.userId) ?? null,
          childSessionCount: childCounts[i],
        })),
      };
    }

    // Admin path: use standard Convex query with .filter() + .paginate()
    let sessionsQuery = ctx.db
      .query("sessions")
      .withIndex("by_parent_session_id", (q) =>
        q.eq("parentSessionId", undefined),
      )
      .order("desc");

    if (excludeUserIds.size > 0) {
      for (const id of excludeUserIds) {
        sessionsQuery = sessionsQuery.filter((q) =>
          q.neq(q.field("userId"), id),
        );
      }
    }
    if (args.excludeUnknownUsers) {
      sessionsQuery = sessionsQuery.filter((q) =>
        q.or(...allUsers.map((u) => q.eq(q.field("userId"), u.workosId))),
      );
    }
    if (excludeProjects.size > 0) {
      for (const project of excludeProjects) {
        sessionsQuery = sessionsQuery.filter((q) =>
          q.and(
            q.neq(q.field("project"), project),
            q.neq(q.field("gitRemote"), project),
            q.neq(q.field("directory"), project),
          ),
        );
      }
    }
    if (args.hasUpload !== undefined) {
      sessionsQuery = args.hasUpload
        ? sessionsQuery.filter((q) => q.neq(q.field("upload"), undefined))
        : sessionsQuery.filter((q) => q.eq(q.field("upload"), undefined));
    }

    const paginatedSessions = await sessionsQuery.paginate(
      args.paginationOpts,
    );

    const childCounts = await Promise.all(
      paginatedSessions.page.map(async (session) => {
        const children = await ctx.db
          .query("sessions")
          .withIndex("by_parent_session_id", (q) =>
            q.eq("parentSessionId", session.sessionId),
          )
          .collect();
        return children.length;
      }),
    );

    return {
      ...paginatedSessions,
      page: paginatedSessions.page.map((session, i) => ({
        ...session,
        user: userMap.get(session.userId) ?? null,
        childSessionCount: childCounts[i],
      })),
    };
  },
});

export const getSession = query({
  args: { sessionId: v.string() },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) {
      return null;
    }

    const session = await ctx.db
      .query("sessions")
      .withIndex("by_session_id", (q) => q.eq("sessionId", args.sessionId))
      .first();

    if (!session) return null;

    const consentFilter =
      auth.role === "reader" ? await buildConsentFilter(ctx) : null;

    // For readers: child sessions inherit parent visibility.
    // If this is a child, check the parent's consent instead.
    if (consentFilter) {
      if (session.parentSessionId) {
        const parent = await ctx.db
          .query("sessions")
          .withIndex("by_session_id", (q) =>
            q.eq("sessionId", session.parentSessionId!),
          )
          .first();
        if (!parent || !consentFilter(parent)) return null;
      } else {
        if (!consentFilter(session)) return null;
      }
    }

    const contentUrl = session.upload
      ? await ctx.storage.getUrl(session.upload.storageId)
      : null;

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", session.userId))
      .first();

    const userSessions = user
      ? await ctx.db
          .query("sessions")
          .withIndex("by_user_id", (q) => q.eq("userId", session.userId))
          .collect()
      : [];

    // For readers, only count consent-visible parent sessions in user stats.
    // Child sessions are not counted independently — they inherit parent visibility.
    const visibleUserSessions = consentFilter
      ? userSessions.filter(
          (s) => !s.parentSessionId && consentFilter(s),
        )
      : userSessions;

    const userWithStats = user
      ? {
          ...user,
          sessionCount: visibleUserSessions.length,
          uploadCount: visibleUserSessions.filter((s) => s.upload).length,
        }
      : null;

    const parentSession = session.parentSessionId
      ? await ctx.db
          .query("sessions")
          .withIndex("by_session_id", (q) =>
            q.eq("sessionId", session.parentSessionId!),
          )
          .first()
      : null;

    // Child sessions always inherit parent visibility — no individual filtering
    const childSessions = await ctx.db
      .query("sessions")
      .withIndex("by_parent_session_id", (q) =>
        q.eq("parentSessionId", args.sessionId),
      )
      .collect();

    return {
      session,
      contentUrl,
      user: userWithStats,
      parentSession,
      childSessions,
    };
  },
});

/** Bulk fetch content URLs for multiple sessions and their agents. Runs buildConsentFilter once. */
export const getSessionContentUrls = query({
  args: { sessionIds: v.array(v.string()) },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return [];

    const consentFilter =
      auth.role === "reader" ? await buildConsentFilter(ctx) : null;

    const results: Array<{ sessionId: string; contentUrl: string }> = [];

    for (const sessionId of args.sessionIds) {
      const session = await ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) => q.eq("sessionId", sessionId))
        .first();

      if (!session?.upload) continue;

      // For readers: check consent (child sessions check parent)
      if (consentFilter) {
        if (session.parentSessionId) {
          const parent = await ctx.db
            .query("sessions")
            .withIndex("by_session_id", (q) =>
              q.eq("sessionId", session.parentSessionId!),
            )
            .first();
          if (!parent || !consentFilter(parent)) continue;
        } else {
          if (!consentFilter(session)) continue;
        }
      }

      const contentUrl = await ctx.storage.getUrl(session.upload.storageId);
      if (contentUrl) {
        results.push({ sessionId, contentUrl });
      }

      // Include uploaded agent sessions for parent sessions
      if (!session.parentSessionId) {
        const children = await ctx.db
          .query("sessions")
          .withIndex("by_parent_session_id", (q) =>
            q.eq("parentSessionId", sessionId),
          )
          .collect();

        for (const child of children) {
          if (!child.upload) continue;
          const childUrl = await ctx.storage.getUrl(child.upload.storageId);
          if (childUrl) {
            results.push({ sessionId: child.sessionId, contentUrl: childUrl });
          }
        }
      }
    }

    return results;
  },
});

export const getUser = query({
  args: { workosId: v.string() },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return null;

    return await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", args.workosId))
      .first();
  },
});

export const listUsers = query({
  args: { paginationOpts: paginationOptsValidator },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) {
      return { page: [], isDone: true, continueCursor: "" };
    }

    const paginatedUsers = await ctx.db
      .query("users")
      .order("desc")
      .paginate(args.paginationOpts);

    return paginatedUsers;
  },
});

export const getUserSessions = query({
  args: {
    paginationOpts: paginationOptsValidator,
    userId: v.string(),
  },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) {
      return { page: [], isDone: true, continueCursor: "" };
    }

    const consentFilter =
      auth.role === "reader" ? await buildConsentFilter(ctx) : null;

    if (consentFilter) {
      // For readers: use stream+filterWith for correct pre-pagination filtering
      const paginatedSessions = await stream(ctx.db, schema)
        .query("sessions")
        .withIndex("by_user_id", (q) => q.eq("userId", args.userId))
        .order("desc")
        .filterWith(async (session) => {
          if (session.parentSessionId) return false;
          return consentFilter(session);
        })
        .paginate(args.paginationOpts);

      const childCounts = await Promise.all(
        paginatedSessions.page.map(async (session) => {
          const children = await ctx.db
            .query("sessions")
            .withIndex("by_parent_session_id", (q) =>
              q.eq("parentSessionId", session.sessionId),
            )
            .collect();
          return children.length;
        }),
      );

      return {
        ...paginatedSessions,
        page: paginatedSessions.page.map((session, i) => ({
          ...session,
          childSessionCount: childCounts[i],
        })),
      };
    }

    // Admin path: standard pagination
    const paginatedSessions = await ctx.db
      .query("sessions")
      .withIndex("by_user_id", (q) => q.eq("userId", args.userId))
      .order("desc")
      .paginate(args.paginationOpts);

    const filteredPage = paginatedSessions.page.filter(
      (s) => !s.parentSessionId,
    );

    const childCounts = await Promise.all(
      filteredPage.map(async (session) => {
        const children = await ctx.db
          .query("sessions")
          .withIndex("by_parent_session_id", (q) =>
            q.eq("parentSessionId", session.sessionId),
          )
          .collect();
        return children.length;
      }),
    );

    return {
      ...paginatedSessions,
      page: filteredPage.map((session, i) => ({
        ...session,
        childSessionCount: childCounts[i],
      })),
    };
  },
});

// --- Backfill (internal only, unchanged) ---

export const updateSessionSummary = internalMutation({
  args: {
    sessionId: v.id("sessions"),
    summary: v.string(),
  },
  handler: async (ctx, { sessionId, summary }) => {
    await ctx.db.patch(sessionId, { summary });
  },
});

function extractSummaryFromEntries(entries: KnownEntry[]): string | undefined {
  const summaries = entries.filter(
    (e): e is KnownEntry & { type: "summary" } => e.type === "summary",
  );
  if (summaries.length > 0) {
    return summaries.at(-1)!.summary;
  }
  for (const entry of entries) {
    if (entry.type !== "user") continue;
    const content = entry.message.content;
    if (!content) continue;
    let text: string | undefined;
    if (typeof content === "string") {
      text = content;
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (!isKnownContentBlock(block)) continue;
        if (block.type === "text" && "text" in block) {
          text = block.text;
          break;
        }
      }
    }
    if (text) {
      const trimmed = text.trim();
      if (trimmed.startsWith("<")) continue;
      const firstLine = trimmed.split("\n")[0].trim();
      if (firstLine) {
        return firstLine.length > 100
          ? `${firstLine.slice(0, 97)}...`
          : firstLine;
      }
    }
  }
  return undefined;
}

export const backfillSummaries = internalAction({
  args: {},
  handler: async (
    ctx,
  ): Promise<{ updated: number; skipped: number; total: number }> => {
    const sessions = (await ctx.runQuery(
      internal.authorized.sessionsNeedingBackfill,
    )) as Array<{
      _id: Id<"sessions">;
      upload: { storageId: Id<"_storage"> };
    }>;

    let updated = 0;
    let skipped = 0;

    for (const session of sessions) {
      const url = await ctx.storage.getUrl(session.upload.storageId);
      if (!url) {
        skipped++;
        continue;
      }

      const response = await fetch(url);
      const text = await response.text();
      const entries: KnownEntry[] = [];

      for (const line of text.split("\n")) {
        if (!line.trim()) continue;
        try {
          const parsed = JSON.parse(line);
          const result = parseKnownEntry(parsed);
          if (result.data) entries.push(result.data);
        } catch {
          // skip unparseable lines
        }
      }

      const summary = extractSummaryFromEntries(entries);
      if (summary) {
        await ctx.runMutation(internal.authorized.updateSessionSummary, {
          sessionId: session._id,
          summary,
        });
        updated++;
      } else {
        skipped++;
      }
    }

    return { updated, skipped, total: sessions.length };
  },
});

export const sessionsNeedingBackfill = internalQuery({
  args: {},
  handler: async (ctx) => {
    const sessions = await ctx.db.query("sessions").collect();
    return sessions.filter((s) => s.upload && !s.summary);
  },
});
