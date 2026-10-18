/**
 * One user's project consent: load their projectConsent rows, normalize identifiers, and group
 * them by project identity (groupProjectConsentEvents in session-data). buildConsentFilter does
 * the same grouping for every user at once, so it doesn't go through here.
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
}> {
  const rawEvents = await ctx.db
    .query("projectConsent")
    .withIndex("by_user_id", (q) => q.eq("userId", userId))
    .collect();

  const normalized: ProjectConsentEvent[] = rawEvents.map((e) => ({
    ...extractIdentifiers(e),
    sessionSharing: e.sessionSharing,
    timestamp: e._creationTime,
  }));

  return groupProjectConsentEvents(normalized);
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

/** Each project group's identifiers and current sharing state (its latest event). */
export function summarizeProjectGroups(groups: ProjectGroup[]) {
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
