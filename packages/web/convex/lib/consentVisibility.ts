/**
 * Canonical consent visibility filter for reader access.
 * This is the SINGLE source of truth for determining which sessions
 * a reader can see. All read queries in authorized.ts must use this.
 *
 * See consent-windows.ts in session-data for the window computation logic.
 */
import type { QueryCtx } from "../_generated/server";
import {
  computeConsentWindows,
  isInConsentWindow,
  type ConsentWindow,
} from "@alignment-hive/session-data";

interface SessionForVisibility {
  userId: string; // workosId
  project: string;
  lastModified?: number;
  upload?: { uploadedAt: number };
}

/**
 * Build a consent filter function for reader access.
 *
 * Loads all consent events, pre-computes consent windows per user (global)
 * and per user+project. Returns a function that checks whether a session
 * is visible to a reader based on consent windows.
 *
 * A session is visible if its timestamp (lastModified, or upload.uploadedAt
 * as fallback) falls within BOTH a global and project consent window.
 */
export async function buildConsentFilter(
  ctx: QueryCtx,
  preloadedUsers?: Array<{ _id: string; workosId: string }>,
): Promise<(session: SessionForVisibility) => boolean> {
  // Use pre-loaded users if available, otherwise load
  const allUsers =
    preloadedUsers ?? (await ctx.db.query("users").collect());
  const workosToDocId = new Map(allUsers.map((u) => [u.workosId, u._id]));

  // Load all consent events
  const allGlobalConsent = await ctx.db
    .query("dataSharingConsent")
    .collect();
  const allProjectConsent = await ctx.db.query("projectConsent").collect();

  // Pre-compute global consent windows per user (by Convex doc ID)
  const globalWindowsByUser = new Map<string, ConsentWindow[]>();
  const globalEventsByUser = new Map<
    string,
    Array<{ sessionSharing: boolean; consentedAt: number }>
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
      consentedAt: event.consentedAt,
    });
  }

  for (const [userId, events] of globalEventsByUser) {
    globalWindowsByUser.set(userId, computeConsentWindows(events));
  }

  // Pre-compute project consent windows per user+project
  const projectWindowsByKey = new Map<string, ConsentWindow[]>();
  const projectEventsByKey = new Map<
    string,
    Array<{ sessionSharing: boolean; consentedAt: number }>
  >();

  for (const event of allProjectConsent) {
    const key = `${event.userId}:${event.project}`;
    let events = projectEventsByKey.get(key);
    if (!events) {
      events = [];
      projectEventsByKey.set(key, events);
    }
    events.push({
      sessionSharing: event.sessionSharing,
      consentedAt: event.consentedAt,
    });
  }

  for (const [key, events] of projectEventsByKey) {
    projectWindowsByKey.set(key, computeConsentWindows(events));
  }

  return (session: SessionForVisibility): boolean => {
    if (!session.upload) return false;

    // Determine the timestamp to check: prefer lastModified, fall back to uploadedAt
    const timestamp = session.lastModified ?? session.upload.uploadedAt;

    // Map workosId to Convex doc ID
    const docId = workosToDocId.get(session.userId);
    if (!docId) return false;

    // Check global consent windows
    const globalWindows = globalWindowsByUser.get(docId);
    if (!globalWindows || !isInConsentWindow(timestamp, globalWindows)) {
      return false;
    }

    // Check project consent windows
    const projectKey = `${docId}:${session.project}`;
    const projectWindows = projectWindowsByKey.get(projectKey);
    if (!projectWindows || !isInConsentWindow(timestamp, projectWindows)) {
      return false;
    }

    return true;
  };
}
