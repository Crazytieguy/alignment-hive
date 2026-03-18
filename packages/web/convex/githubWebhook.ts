"use node";

import { v } from "convex/values";
import { internalAction } from "./_generated/server";
import { internal } from "./_generated/api";
import { createHmac, timingSafeEqual } from "node:crypto";

/** Process a GitHub webhook. Verifies signature and dispatches to mutations. */
export const handleWebhook = internalAction({
  args: {
    rawBody: v.string(),
    signature: v.string(),
    event: v.string(),
  },
  handler: async (ctx, { rawBody, signature, event }) => {
    const secret = process.env.GITHUB_WEBHOOK_SECRET;
    if (!secret) {
      throw new Error("GITHUB_WEBHOOK_SECRET not configured");
    }

    // Verify HMAC signature
    const expected =
      "sha256=" + createHmac("sha256", secret).update(rawBody).digest("hex");

    const sigBuf = Buffer.from(signature);
    const expBuf = Buffer.from(expected);
    if (
      sigBuf.length !== expBuf.length ||
      !timingSafeEqual(sigBuf, expBuf)
    ) {
      throw new Error("Invalid webhook signature");
    }

    const payload = JSON.parse(rawBody);
    const action = payload.action as string;

    if (event === "installation") {
      const installation = payload.installation;
      const installationId = installation.id as number;
      const accountLogin = (
        installation.account.login as string
      ).toLowerCase();
      const accountId = installation.account.id as number;

      if (action === "created") {
        await ctx.runMutation(internal.github.upsertGithubInstallation, {
          installationId,
          accountLogin,
          accountId,
        });

        const repos = (payload.repositories ?? []) as Array<{
          id: number;
          full_name: string;
        }>;
        if (repos.length > 0) {
          await ctx.runMutation(internal.github.batchUpsertLinkedRepos, {
            installationId,
            repos: repos.map((r) => ({
              repoId: r.id,
              gitRemote: `github.com/${r.full_name}`.toLowerCase(),
            })),
          });
        }
      } else if (action === "deleted") {
        await ctx.runMutation(internal.github.removeInstallation, {
          installationId,
        });
      }
    } else if (event === "installation_repositories") {
      const installationId = payload.installation.id as number;

      if (action === "added") {
        const repos = (payload.repositories_added ?? []) as Array<{
          id: number;
          full_name: string;
        }>;
        if (repos.length > 0) {
          await ctx.runMutation(internal.github.batchUpsertLinkedRepos, {
            installationId,
            repos: repos.map((r) => ({
              repoId: r.id,
              gitRemote: `github.com/${r.full_name}`.toLowerCase(),
            })),
          });
        }
      } else if (action === "removed") {
        const repos = (payload.repositories_removed ?? []) as Array<{
          id: number;
        }>;
        if (repos.length > 0) {
          await ctx.runMutation(internal.github.batchRemoveLinkedRepos, {
            repoIds: repos.map((r) => r.id),
          });
        }
      }
    }
  },
});
