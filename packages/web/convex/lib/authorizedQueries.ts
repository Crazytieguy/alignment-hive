/**
 * Shared query implementations for authorized data access.
 * Called by both public Convex queries (JWT auth) and internal queries (API key auth via HTTP).
 * Consent filtering is always applied — these functions never skip it.
 */
import type { QueryCtx } from "../_generated/server";
import type { Doc, Id } from "../_generated/dataModel";
import type { PaginationOptions } from "convex/server";
import { stream } from "convex-helpers/server/stream";
import schema from "../schema";
import { buildConsentFilter } from "./consentVisibility";
import { loadAndGroupUserConsent } from "./projectConsent";

// --- Formatting helpers ---

function formatUser(user: Doc<"users">) {
  return {
    userId: user._id as string,
    email: user.email,
    firstName: user.firstName,
    lastName: user.lastName,
  };
}

async function formatUpload(
  ctx: QueryCtx,
  upload: Doc<"sessions">["upload"],
) {
  if (!upload) return undefined;
  const contentUrl = await ctx.storage.getUrl(upload.storageId);
  if (!contentUrl) return undefined;
  return { contentUrl, uploadedAt: upload.uploadedAt };
}

function formatSessionBase(
  session: Doc<"sessions">,
  upload: { contentUrl: string; uploadedAt: number } | undefined,
) {
  return {
    sessionId: session.sessionId,
    directory: session.directory,
    gitRemote: session.gitRemote,
    lineCount: session.lineCount,
    lastModified: session.lastModified,
    lastHeartbeat: session.lastHeartbeat,
    summary: session.summary,
    sessionStartGitCommitHash: session.sessionStartGitCommitHash,
    parentSessionId: session.parentSessionId,
    upload,
  };
}

async function formatAgentSessions(ctx: QueryCtx, parentSessionId: string) {
  const children = await ctx.db
    .query("sessions")
    .withIndex("by_parent_session_id", (q) =>
      q.eq("parentSessionId", parentSessionId),
    )
    .collect();

  return await Promise.all(
    children.map(async (child) => {
      const childUpload = await formatUpload(ctx, child.upload);
      return {
        sessionId: child.sessionId,
        lineCount: child.lineCount,
        lastModified: child.lastModified,
        lastHeartbeat: child.lastHeartbeat,
        summary: child.summary,
        parentSessionId: child.parentSessionId!,
        upload: childUpload,
      };
    }),
  );
}

async function resolveSessionUser(
  ctx: QueryCtx,
  session: Doc<"sessions">,
): Promise<Doc<"users"> | null> {
  if (session.userDocId) {
    return await ctx.db.get(session.userDocId);
  }
  if (session.userId) {
    return await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", session.userId!))
      .first();
  }
  return null;
}

// --- Filter type (shared between public and internal queries) ---

export type SessionFilter =
  | {
      type: "include";
      userId: Id<"users">;
      project?: { directory: string } | { gitRemote: string };
      hasUpload?: boolean;
    }
  | {
      type: "exclude";
      excludeUserIds?: Array<Id<"users">>;
      excludeDirectories?: string[];
      excludeGitRemotes?: string[];
      hasUpload?: boolean;
    };

// --- Query implementations ---

export async function listSessionsImpl(
  ctx: QueryCtx,
  args: { paginationOpts: PaginationOptions; filter?: SessionFilter },
) {
  const allUsers = await ctx.db.query("users").collect();
  const consentFilter = await buildConsentFilter(ctx, allUsers);

  const filter = args.filter;
  const hasUpload = filter?.hasUpload;

  if (filter?.type === "include") {
    const projectDir =
      filter.project && "directory" in filter.project
        ? filter.project.directory
        : undefined;
    const projectRemote =
      filter.project && "gitRemote" in filter.project
        ? filter.project.gitRemote
        : undefined;

    const paginatedSessions = await stream(ctx.db, schema)
      .query("sessions")
      .withIndex("by_user_doc_id", (q) => q.eq("userDocId", filter.userId))
      .order("desc")
      .filterWith(async (session) => {
        if (session.parentSessionId) return false;
        if (hasUpload !== undefined) {
          if (hasUpload && !session.upload) return false;
          if (!hasUpload && session.upload) return false;
        }
        if (projectDir && session.directory !== projectDir) return false;
        if (projectRemote && session.gitRemote !== projectRemote) return false;
        return consentFilter(session);
      })
      .paginate(args.paginationOpts);

    const user = await ctx.db.get(filter.userId);

    return {
      ...paginatedSessions,
      page: await Promise.all(
        paginatedSessions.page.map(async (session) => {
          const upload = await formatUpload(ctx, session.upload);
          const agentSessions = await formatAgentSessions(
            ctx,
            session.sessionId,
          );
          return {
            ...formatSessionBase(session, upload),
            user: user ? formatUser(user) : null,
            agentSessions,
          };
        }),
      ),
    };
  }

  // Exclude path (default) — build user maps for resolving session owners
  const userDocMap = new Map(allUsers.map((u) => [u._id as string, u]));
  const workosToUser = new Map(allUsers.map((u) => [u.workosId, u]));
  const excludeUserIds = new Set(
    filter?.type === "exclude"
      ? (filter.excludeUserIds ?? []).map(String)
      : [],
  );
  const excludeDirectories = new Set(
    filter?.type === "exclude" ? (filter.excludeDirectories ?? []) : [],
  );
  const excludeGitRemotes = new Set(
    filter?.type === "exclude" ? (filter.excludeGitRemotes ?? []) : [],
  );

  const paginatedSessions = await stream(ctx.db, schema)
    .query("sessions")
    .withIndex("by_parent_session_id", (q) =>
      q.eq("parentSessionId", undefined),
    )
    .order("desc")
    .filterWith(async (session) => {
      if (hasUpload !== undefined) {
        if (hasUpload && !session.upload) return false;
        if (!hasUpload && session.upload) return false;
      }
      if (session.userDocId && excludeUserIds.has(session.userDocId)) {
        return false;
      }
      if (session.directory && excludeDirectories.has(session.directory)) {
        return false;
      }
      if (session.gitRemote && excludeGitRemotes.has(session.gitRemote)) {
        return false;
      }
      return consentFilter(session);
    })
    .paginate(args.paginationOpts);

  return {
    ...paginatedSessions,
    page: await Promise.all(
      paginatedSessions.page.map(async (session) => {
        const upload = await formatUpload(ctx, session.upload);
        const user =
          (session.userDocId
            ? userDocMap.get(session.userDocId)
            : session.userId
              ? workosToUser.get(session.userId)
              : null) ?? null;
        const agentSessions = await formatAgentSessions(
          ctx,
          session.sessionId,
        );
        return {
          ...formatSessionBase(session, upload),
          user: user ? formatUser(user) : null,
          agentSessions,
        };
      }),
    ),
  };
}

export async function getSessionImpl(
  ctx: QueryCtx,
  args: { sessionId: string },
) {
  const session = await ctx.db
    .query("sessions")
    .withIndex("by_session_id", (q) => q.eq("sessionId", args.sessionId))
    .first();

  if (!session) return null;

  const consentFilter = await buildConsentFilter(ctx);

  // Fetch parent once — reused for both consent check and return value
  const parentSession = session.parentSessionId
    ? await ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) =>
          q.eq("sessionId", session.parentSessionId!),
        )
        .first()
    : null;

  // Child sessions inherit parent visibility
  if (session.parentSessionId) {
    if (!parentSession || !consentFilter(parentSession)) return null;
  } else {
    if (!consentFilter(session)) return null;
  }

  const upload = await formatUpload(ctx, session.upload);
  const user = await resolveSessionUser(ctx, session);
  const agentSessions = await formatAgentSessions(ctx, session.sessionId);

  return {
    ...formatSessionBase(session, upload),
    user: user ? formatUser(user) : null,
    agentSessions,
    parentSession: parentSession
      ? {
          sessionId: parentSession.sessionId,
          summary: parentSession.summary,
          directory: parentSession.directory,
          gitRemote: parentSession.gitRemote,
        }
      : undefined,
  };
}

export async function getUserImpl(
  ctx: QueryCtx,
  args: { userId: Id<"users"> },
) {
  const user = await ctx.db.get(args.userId);
  if (!user) return null;

  const consentFilter = await buildConsentFilter(ctx);

  const userSessions = await ctx.db
    .query("sessions")
    .withIndex("by_user_doc_id", (q) => q.eq("userDocId", args.userId))
    .collect();

  const legacySessions = await ctx.db
    .query("sessions")
    .withIndex("by_user_id", (q) => q.eq("userId", user.workosId))
    .collect();

  const allSessions = new Map<string, Doc<"sessions">>();
  for (const s of [...userSessions, ...legacySessions]) {
    allSessions.set(s.sessionId, s);
  }

  const visibleParents = [...allSessions.values()].filter(
    (s) => !s.parentSessionId && consentFilter(s),
  );

  return {
    ...formatUser(user),
    sessionCount: visibleParents.length,
    uploadCount: visibleParents.filter((s) => s.upload).length,
  };
}

export async function listUsersImpl(
  ctx: QueryCtx,
  args: { paginationOpts: PaginationOptions },
) {
  const allConsent = await ctx.db.query("dataSharingConsent").collect();
  const consentedUserIds = new Set(
    allConsent
      .filter((c) => c.sessionSharing)
      .map((c) => c.userId as string),
  );

  const paginatedUsers = await stream(ctx.db, schema)
    .query("users")
    .order("desc")
    .filterWith(async (user) => consentedUserIds.has(user._id))
    .paginate(args.paginationOpts);

  return {
    ...paginatedUsers,
    page: paginatedUsers.page.map(formatUser),
  };
}

export async function listProjectsImpl(
  ctx: QueryCtx,
  args: { userId: Id<"users"> },
) {
  const { groups } = await loadAndGroupUserConsent(ctx, args.userId);

  return groups.map((group) => {
    const latest = group.events.reduce((a, b) =>
      a.timestamp > b.timestamp ? a : b,
    );
    return {
      directories: [...group.directories],
      gitRemotes: [...group.gitRemotes],
      sessionSharing: latest.sessionSharing,
      latestAt: latest.timestamp,
    };
  });
}
