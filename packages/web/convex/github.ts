import { v } from "convex/values";
import { internalMutation, query } from "./_generated/server";

// --- Queries ---

/** Check if a gitRemote is linked (App installed on that repo). Shared infrastructure — not per-user. */
export const getRepoLinkStatus = query({
  args: { gitRemote: v.string() },
  handler: async (ctx, { gitRemote }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return "not-linked" as const;
    }

    const linked = await ctx.db
      .query("linkedRepos")
      .withIndex("by_git_remote", (q) =>
        q.eq("gitRemote", gitRemote.toLowerCase()),
      )
      .first();

    return linked ? ("linked" as const) : ("not-linked" as const);
  },
});

/** Batch check which gitRemotes are linked. Returns the set of linked remotes. */
export const getLinkedRemotesFromList = query({
  args: { gitRemotes: v.array(v.string()) },
  handler: async (ctx, { gitRemotes }) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) return [];

    const linked: string[] = [];
    for (const remote of gitRemotes) {
      const found = await ctx.db
        .query("linkedRepos")
        .withIndex("by_git_remote", (q) =>
          q.eq("gitRemote", remote.toLowerCase()),
        )
        .first();
      if (found) linked.push(remote);
    }
    return linked;
  },
});

// --- Internal mutations (called by webhook handler) ---

export const upsertGithubInstallation = internalMutation({
  args: {
    installationId: v.number(),
    accountLogin: v.string(),
    accountId: v.number(),
  },
  handler: async (ctx, args) => {
    const existing = await ctx.db
      .query("githubInstallations")
      .withIndex("by_installation_id", (q) =>
        q.eq("installationId", args.installationId),
      )
      .first();

    if (existing) {
      await ctx.db.patch(existing._id, {
        accountLogin: args.accountLogin,
        accountId: args.accountId,
        updatedAt: Date.now(),
      });
    } else {
      await ctx.db.insert("githubInstallations", {
        ...args,
        updatedAt: Date.now(),
      });
    }
  },
});

export const removeInstallation = internalMutation({
  args: { installationId: v.number() },
  handler: async (ctx, { installationId }) => {
    const installation = await ctx.db
      .query("githubInstallations")
      .withIndex("by_installation_id", (q) =>
        q.eq("installationId", installationId),
      )
      .first();

    if (installation) {
      await ctx.db.delete(installation._id);
    }

    // Cascade: remove all linked repos for this installation
    const repos = await ctx.db
      .query("linkedRepos")
      .withIndex("by_installation_id", (q) =>
        q.eq("installationId", installationId),
      )
      .collect();

    for (const repo of repos) {
      await ctx.db.delete(repo._id);
    }
  },
});

export const batchUpsertLinkedRepos = internalMutation({
  args: {
    installationId: v.number(),
    repos: v.array(v.object({ repoId: v.number(), gitRemote: v.string() })),
  },
  handler: async (ctx, { installationId, repos }) => {
    for (const { repoId, gitRemote } of repos) {
      const existing = await ctx.db
        .query("linkedRepos")
        .withIndex("by_repo_id", (q) => q.eq("repoId", repoId))
        .first();

      const record = {
        installationId,
        gitRemote: gitRemote.toLowerCase(),
        repoId,
      };

      if (existing) {
        await ctx.db.replace(existing._id, record);
      } else {
        await ctx.db.insert("linkedRepos", record);
      }
    }
  },
});

export const batchRemoveLinkedRepos = internalMutation({
  args: { repoIds: v.array(v.number()) },
  handler: async (ctx, { repoIds }) => {
    for (const repoId of repoIds) {
      const existing = await ctx.db
        .query("linkedRepos")
        .withIndex("by_repo_id", (q) => q.eq("repoId", repoId))
        .first();

      if (existing) {
        await ctx.db.delete(existing._id);
      }
    }
  },
});
