/**
 * Canonical consent visibility filter.
 * This is the SINGLE source of truth for determining which sessions
 * are visible. Every session read (lib/authorizedQueries.ts) must use this.
 *
 * See consent-windows.ts in session-data for the window computation logic.
 */
import type { QueryCtx } from "../_generated/server";
import type { Id } from "../_generated/dataModel";
import {
  computeConsentWindows,
  isInConsentWindow,
  extractIdentifiers,
  groupProjectConsentEvents,
  findGroupForIdentifiers,
  type ConsentWindow,
  type ProjectConsentEvent,
} from "@alignment-hive/session-data";

interface SessionForVisibility {
  userDocId: string;
  project?: string;
  directory?: string;
  gitRemote?: string;
  lastModified?: number;
  upload?: { uploadedAt: number };
}

/**
 * Build a consent filter function.
 *
 * Loads consent events, pre-computes consent windows per user (global)
 * and per project group. Returns a function that checks whether a session
 * is visible based on consent windows.
 *
 * A session is visible if its timestamp (lastModified, or upload.uploadedAt
 * as fallback) falls within BOTH a global and project consent window.
 *
 * Without `owner`, every user's history is loaded, for reads that span users. With it, only
 * that owner's (by index): a user's windows depend on their own events alone, so the filter
 * decides their sessions exactly as the full one does, and fails closed on anyone else's.
 * Only pass it when every session the filter will see belongs to that owner.
 */
export async function buildConsentFilter(
  ctx: QueryCtx,
  owner?: Id<"users">,
): Promise<(session: SessionForVisibility) => boolean> {
  const allGlobalConsent = owner
    ? await ctx.db
        .query("dataSharingConsent")
        .withIndex("by_user_id", (q) => q.eq("userId", owner))
        .collect()
    : await ctx.db.query("dataSharingConsent").collect();
  const allProjectConsent = owner
    ? await ctx.db
        .query("projectConsent")
        .withIndex("by_user_id", (q) => q.eq("userId", owner))
        .collect()
    : await ctx.db.query("projectConsent").collect();

  // Pre-compute global consent windows per user (by Convex doc ID)
  const globalWindowsByUser = new Map<string, ConsentWindow[]>();
  const globalEventsByUser = new Map<
    string,
    Array<{ sessionSharing: boolean; timestamp: number }>
  >();

  for (const event of allGlobalConsent) {
    const userId = event.userId;
    let events = globalEventsByUser.get(userId);
    if (!events) {
      events = [];
      globalEventsByUser.set(userId, events);
    }
    events.push({
      sessionSharing: event.sessionSharing,
      timestamp: event._creationTime,
    });
  }

  for (const [userId, events] of globalEventsByUser) {
    globalWindowsByUser.set(userId, computeConsentWindows(events));
  }

  // Pre-compute project consent windows per user using connected-component grouping
  const projectEventsByUser = new Map<string, ProjectConsentEvent[]>();

  for (const event of allProjectConsent) {
    const userId = event.userId as string;
    let events = projectEventsByUser.get(userId);
    if (!events) {
      events = [];
      projectEventsByUser.set(userId, events);
    }
    const ids = extractIdentifiers(event);
    events.push({
      ...ids,
      sessionSharing: event.sessionSharing,
      timestamp: event._creationTime,
    });
  }

  // Per user: group events, compute windows per group, build lookup
  const userProjectData = new Map<
    string,
    {
      lookup: Map<string, number>;
      groupWindows: Map<number, ConsentWindow[]>;
    }
  >();

  for (const [userId, events] of projectEventsByUser) {
    const { groups, lookup } = groupProjectConsentEvents(events);
    const groupWindows = new Map<number, ConsentWindow[]>();
    for (let i = 0; i < groups.length; i++) {
      groupWindows.set(i, computeConsentWindows(groups[i].events));
    }
    userProjectData.set(userId, { lookup, groupWindows });
  }

  return (session: SessionForVisibility): boolean => {
    if (!session.upload) return false;

    // Determine the timestamp to check: prefer lastModified, fall back to uploadedAt
    const timestamp = session.lastModified ?? session.upload.uploadedAt;

    const docId = session.userDocId;

    // Check global consent windows
    const globalWindows = globalWindowsByUser.get(docId);
    if (!globalWindows || !isInConsentWindow(timestamp, globalWindows)) {
      return false;
    }

    // Check project consent windows using grouped identifiers
    const userData = userProjectData.get(docId);
    if (!userData) return false;

    const sessionIds = extractIdentifiers(session);
    const groupIdx = findGroupForIdentifiers(userData.lookup, sessionIds);
    if (groupIdx === undefined) return false; // no match or ambiguous — fail closed

    const projectWindows = userData.groupWindows.get(groupIdx);
    if (!projectWindows || !isInConsentWindow(timestamp, projectWindows)) {
      return false;
    }

    return true;
  };
}
