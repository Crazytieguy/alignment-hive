/**
 * Shared helpers for loading and grouping project consent events.
 *
 * Consolidates the "load all consent events → group by project identity →
 * find matching group" pattern used by verifyConsent, buildConsentFilter,
 * getEnabledProjects, and getConsentHistory.
 */
import type { QueryCtx, MutationCtx } from "../_generated/server";
import type { Id } from "../_generated/dataModel";
import {
  extractIdentifiers,
  groupProjectConsentEvents,
  findGroupForIdentifiers,
  type ProjectIdentifiers,
  type ProjectConsentEvent,
  type ProjectGroup,
} from "@alignment-hive/session-data";

/**
 * Load all projectConsent events for a user, normalize identifiers, group them.
 */
export async function loadAndGroupUserConsent(
  ctx: QueryCtx | MutationCtx,
  userId: Id<"users">,
): Promise<{
  groups: ProjectGroup[];
  lookup: Map<string, number>;
  allEvents: ProjectConsentEvent[];
}> {
  const rawEvents = await ctx.db
    .query("projectConsent")
    .withIndex("by_user_id", (q) => q.eq("userId", userId))
    .collect();

  const normalized: ProjectConsentEvent[] = rawEvents.map((e) => ({
    ...extractIdentifiers(e),
    sessionSharing: e.sessionSharing,
    consentedAt: e.consentedAt,
  }));

  const { groups, lookup } = groupProjectConsentEvents(normalized);
  return { groups, lookup, allEvents: normalized };
}

/**
 * Find the matching group's events for the given project identifiers.
 * Returns null if no matching group found.
 */
export async function getMatchingConsentGroup(
  ctx: QueryCtx | MutationCtx,
  userId: Id<"users">,
  identifiers: ProjectIdentifiers,
): Promise<ProjectGroup | null> {
  const { groups, lookup } = await loadAndGroupUserConsent(ctx, userId);
  const idx = findGroupForIdentifiers(lookup, identifiers);
  if (idx === undefined) return null;
  return groups[idx];
}
