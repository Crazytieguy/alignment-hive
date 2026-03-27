import { v } from "convex/values";
import { query, action, internalMutation } from "./_generated/server";
import { internal } from "./_generated/api";
import { CURRENT_AGREEMENT_VERSION, hasCurrentAgreement } from "./lib/agreement";

const DATA_ACCESSOR_ROLE_SLUG = "data-accessor";

/**
 * Record that the authenticated user agrees to the data accessor agreement,
 * then grant them the data-accessor role in their WorkOS org (which includes
 * the widgets:api-keys:manage permission for API key creation).
 *
 * If the WorkOS role grant fails, the agreement record is rolled back.
 */
export const submitAgreement = action({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      throw new Error("Not authenticated");
    }

    // 1. Create agreement record (null if already agreed)
    const agreementId = await ctx.runMutation(
      internal.agreement.submitAgreementInternal,
      { workosId: identity.subject },
    );

    // Already agreed — no further action needed
    if (!agreementId) return;

    // 2. Grant WorkOS data-accessor role
    const workosApiKey = process.env.WORKOS_API_KEY;
    if (!workosApiKey) {
      // Can't grant role without API key — roll back
      await ctx.runMutation(internal.agreement.deleteAgreementInternal, {
        agreementId,
      });
      throw new Error("Server configuration error");
    }

    try {
      const membershipsResp = await fetch(
        `https://api.workos.com/user_management/organization_memberships?user_id=${encodeURIComponent(identity.subject)}`,
        { headers: { Authorization: `Bearer ${workosApiKey}` } },
      );

      if (!membershipsResp.ok) {
        throw new Error(
          `Failed to list org memberships: ${membershipsResp.status}`,
        );
      }

      const memberships = (await membershipsResp.json()) as {
        data: Array<{
          id: string;
          organization_id: string;
          status: string;
          role: { slug: string };
        }>;
      };

      for (const membership of memberships.data) {
        if (membership.status !== "active") continue;
        if (membership.role?.slug === DATA_ACCESSOR_ROLE_SLUG) continue;

        const updateResp = await fetch(
          `https://api.workos.com/user_management/organization_memberships/${membership.id}`,
          {
            method: "PUT",
            headers: {
              Authorization: `Bearer ${workosApiKey}`,
              "Content-Type": "application/json",
            },
            body: JSON.stringify({ role_slug: DATA_ACCESSOR_ROLE_SLUG }),
          },
        );

        if (!updateResp.ok) {
          throw new Error(
            `Failed to assign role on membership ${membership.id}: ${updateResp.status}`,
          );
        }
      }
    } catch (error) {
      // Roll back the agreement record
      await ctx.runMutation(internal.agreement.deleteAgreementInternal, {
        agreementId,
      });
      throw error;
    }
  },
});

/** Create agreement record. Returns the document ID for rollback. */
export const submitAgreementInternal = internalMutation({
  args: { workosId: v.string() },
  handler: async (ctx, { workosId }) => {
    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", workosId))
      .first();

    if (!user?.hasDataAccess) {
      throw new Error("Not authorized — no data access");
    }

    // Idempotent — return null if already agreed (so the action skips WorkOS + rollback)
    if (await hasCurrentAgreement(ctx, user._id)) {
      return null;
    }

    return await ctx.db.insert("dataAccessorAgreements", {
      userId: user._id,
      agreementVersion: CURRENT_AGREEMENT_VERSION,
    });
  },
});

/** Delete an agreement record (used for rollback on WorkOS failure). */
export const deleteAgreementInternal = internalMutation({
  args: { agreementId: v.id("dataAccessorAgreements") },
  handler: async (ctx, { agreementId }) => {
    await ctx.db.delete(agreementId);
  },
});

/** Check whether the authenticated user has agreed to the current agreement version. */
export const getAgreementStatus = query({
  args: {},
  handler: async (ctx) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) {
      return { agreement: undefined };
    }

    const user = await ctx.db
      .query("users")
      .withIndex("by_workos_id", (q) => q.eq("workosId", identity.subject))
      .first();

    if (!user) {
      return { agreement: undefined };
    }

    // This query needs the creation time, so it can't use the shared helper
    const agreements = await ctx.db
      .query("dataAccessorAgreements")
      .withIndex("by_user_id", (q) => q.eq("userId", user._id))
      .collect();

    const latest = agreements.find(
      (a) => a.agreementVersion === CURRENT_AGREEMENT_VERSION,
    );

    if (!latest) {
      return { agreement: undefined };
    }

    return {
      agreement: {
        agreedAt: latest._creationTime,
        version: latest.agreementVersion,
      },
    };
  },
});
