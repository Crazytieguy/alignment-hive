"use node";

import { v } from "convex/values";
import { internalAction } from "./_generated/server";
import { internal } from "./_generated/api";
import { createHmac, timingSafeEqual } from "node:crypto";
import { z } from "zod/v4";

const repoSchema = z.object({
  id: z.number(),
  full_name: z.string(),
});

const installationEventSchema = z.object({
  action: z.string(),
  installation: z.object({
    id: z.number(),
    account: z.object({
      login: z.string(),
      id: z.number(),
    }),
  }),
  repositories: z.array(repoSchema).optional().default([]),
});

const repoEventSchema = z.object({
  action: z.string(),
  installation: z.object({ id: z.number() }),
  repositories_added: z.array(repoSchema).optional().default([]),
  repositories_removed: z.array(z.object({ id: z.number() }))
    .optional()
    .default([]),
});

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

    const raw = JSON.parse(rawBody);

    if (event === "installation") {
      const payload = installationEventSchema.parse(raw);
      const { installation, action } = payload;
      const installationId = installation.id;
      const accountLogin = installation.account.login.toLowerCase();
      const accountId = installation.account.id;

      if (action === "created") {
        await ctx.runMutation(internal.github.upsertGithubInstallation, {
          installationId,
          accountLogin,
          accountId,
        });

        if (payload.repositories.length > 0) {
          await ctx.runMutation(internal.github.batchUpsertLinkedRepos, {
            installationId,
            repos: payload.repositories.map((r) => ({
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
      const payload = repoEventSchema.parse(raw);
      const installationId = payload.installation.id;

      if (payload.action === "added") {
        if (payload.repositories_added.length > 0) {
          await ctx.runMutation(internal.github.batchUpsertLinkedRepos, {
            installationId,
            repos: payload.repositories_added.map((r) => ({
              repoId: r.id,
              gitRemote: `github.com/${r.full_name}`.toLowerCase(),
            })),
          });
        }
      } else if (payload.action === "removed") {
        if (payload.repositories_removed.length > 0) {
          await ctx.runMutation(internal.github.batchRemoveLinkedRepos, {
            repoIds: payload.repositories_removed.map((r) => r.id),
          });
        }
      }
    }
  },
});
