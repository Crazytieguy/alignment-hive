import { readFile } from 'node:fs/promises';
import { checkAuthStatus } from './auth';
import { getProjectIdentifiers, matchesProject } from './config';
import { getConsentStatus, getProjectSharing } from './convex';
import { parseJsonl, transformEntry } from './extraction';
import {
  CONSENT_REVIEW_PERIOD_MS,
  SESSION_REVIEW_PERIOD_MS,
} from './session-state';
import { extractSessionSummary } from './summary';
import type { KnownEntry } from '@alignment-hive/session-data';
import type { DiscoveredSession, checkSessionEligibility } from './session-state';

/** Resolve the project consent mtime, or null if offline/unauthorized. */
export async function getProjectConsentMtime(cwd: string): Promise<number | null> {
  try {
    const status = await checkAuthStatus(true);
    if (!status.authenticated) return null;
    const [consent, allProjects] = await Promise.all([
      getConsentStatus(),
      getProjectSharing(),
    ]);
    if (!consent?.hasConsent || !consent.sessionSharing) return null;
    const ids = getProjectIdentifiers(cwd);
    const projectConsent = matchesProject(allProjects, ids);
    return projectConsent?.sessionSharing ? projectConsent.latestAt : null;
  } catch {
    return null;
  }
}

/** Get a summary from the first entries of a session file. */
export async function getSessionSummary(sessionPath: string): Promise<string> {
  try {
    const content = await readFile(sessionPath, 'utf-8');
    const entries: Array<KnownEntry> = [];
    let count = 0;
    for (const rawEntry of parseJsonl(content)) {
      const { entry } = transformEntry(rawEntry);
      if (entry) {
        entries.push(entry as KnownEntry);
        count++;
        if (count >= 20) break;
      }
    }
    return extractSessionSummary(entries) || '';
  } catch {
    return '';
  }
}

/** Map eligibility result to a display status string. */
export function getDisplayStatus(
  result: ReturnType<typeof checkSessionEligibility>,
  session: DiscoveredSession,
  consentMtime: number | null,
  snoozeUntil: number | null,
): string {
  if (!result.eligible) {
    switch (result.reason) {
      case 'excluded':
        return 'excluded';
      case 'already uploaded':
        return 'uploaded';
      case 'pending review':
      case 'consent review period': {
        const mtimeMs = session.mtime.getTime();
        const sessionEligibleAt = mtimeMs + SESSION_REVIEW_PERIOD_MS;
        const consentEligibleAt = (consentMtime ?? 0) + CONSENT_REVIEW_PERIOD_MS;
        const eligibleAt = Math.max(sessionEligibleAt, consentEligibleAt);
        const remainingMs = eligibleAt - Date.now();
        const remainingHours = Math.max(0, Math.ceil(remainingMs / (60 * 60 * 1000)));
        return `pending:${remainingHours}h`;
      }
      default:
        return result.reason;
    }
  }
  return snoozeUntil ? 'snoozed' : 'ready';
}
