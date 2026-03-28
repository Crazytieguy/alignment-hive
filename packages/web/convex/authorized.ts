/**
 * Authorized session data queries. All public queries call requireAuthorized() for JWT auth.
 * Internal query variants are used by HTTP API endpoints (API key auth handled externally).
 * Both paths call shared *Impl functions from lib/authorizedQueries.ts for identical
 * consent filtering and data shaping.
 *
 * See CLAUDE.md "Session Data Access Control" for the full security model.
 */
import { paginationOptsValidator } from "convex/server";
import { v } from "convex/values";
import {
  parseKnownEntry,
  extractSessionSummary,
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
import { requireAuthorized } from "./lib/auth";
import {
  listSessionsImpl,
  getSessionImpl,
  getUserImpl,
  listUsersImpl,
  listProjectsImpl,
} from "./lib/authorizedQueries";

// --- Shared Convex validators ---

const sessionScopeValidator = v.optional(
  v.union(
    v.object({
      type: v.literal("include"),
      userId: v.id("users"),
      project: v.optional(
        v.union(
          v.object({ directory: v.string() }),
          v.object({ gitRemote: v.string() }),
        ),
      ),
    }),
    v.object({
      type: v.literal("exclude"),
      excludeUserIds: v.optional(v.array(v.id("users"))),
      excludeDirectories: v.optional(v.array(v.string())),
      excludeGitRemotes: v.optional(v.array(v.string())),
    }),
  ),
);

const paginationOptsArg = v.object({
  numItems: v.number(),
  cursor: v.union(v.string(), v.null()),
});

// --- Public queries (JWT auth via requireAuthorized) ---

export const listSessions = query({
  args: {
    paginationOpts: paginationOptsValidator,
    scope: sessionScopeValidator,
    hasUpload: v.optional(v.boolean()),
  },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return { page: [], isDone: true, continueCursor: "" };
    return listSessionsImpl(ctx, args);
  },
});

export const getSession = query({
  args: { sessionId: v.string() },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return null;
    return getSessionImpl(ctx, args);
  },
});

export const getUser = query({
  args: { userId: v.id("users") },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return null;
    return getUserImpl(ctx, args);
  },
});

export const listUsers = query({
  args: { paginationOpts: paginationOptsValidator },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return { page: [], isDone: true, continueCursor: "" };
    return listUsersImpl(ctx, args);
  },
});

export const listProjects = query({
  args: { userId: v.id("users") },
  handler: async (ctx, args) => {
    const auth = await requireAuthorized(ctx);
    if (!auth) return [];
    return listProjectsImpl(ctx, args);
  },
});

// --- Internal query variants (for HTTP API — auth handled by Hono middleware) ---

export const listSessionsInternal = internalQuery({
  args: { paginationOpts: paginationOptsArg, scope: sessionScopeValidator, hasUpload: v.optional(v.boolean()) },
  handler: async (ctx, args) => listSessionsImpl(ctx, args),
});

export const getSessionInternal = internalQuery({
  args: { sessionId: v.string() },
  handler: async (ctx, args) => getSessionImpl(ctx, args),
});

export const getUserInternal = internalQuery({
  args: { userId: v.id("users") },
  handler: async (ctx, args) => getUserImpl(ctx, args),
});

export const listUsersInternal = internalQuery({
  args: { paginationOpts: paginationOptsArg },
  handler: async (ctx, args) => listUsersImpl(ctx, args),
});

export const listProjectsInternal = internalQuery({
  args: { userId: v.id("users") },
  handler: async (ctx, args) => listProjectsImpl(ctx, args),
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

      const summary = extractSessionSummary(entries);
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
