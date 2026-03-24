import { computeEligibleAt } from './session-state';
import type { DiscoveredSession, checkSessionEligibility } from './session-state';

export type DisplayStatus =
  | { type: 'excluded' }
  | { type: 'uploaded' }
  | { type: 'ready' }
  | { type: 'snoozed' }
  | { type: 'pending'; remainingHours: number };

export interface DisplayContext {
  consentMtime: number | null;
  snoozeUntil: number | null;
  migrationTimestamp?: number | null;
}

/** Map eligibility result to a structured display status. */
export function getDisplayStatus(
  result: ReturnType<typeof checkSessionEligibility>,
  session: DiscoveredSession,
  ctx: DisplayContext,
): DisplayStatus {
  const { consentMtime, snoozeUntil, migrationTimestamp } = ctx;
  if (!result.eligible) {
    switch (result.reason) {
      case 'excluded':
        return { type: 'excluded' };
      case 'already uploaded':
        return { type: 'uploaded' };
      case 'pending review':
      case 'consent review period': {
        const eligibleAt = computeEligibleAt(session.mtime.getTime(), consentMtime ?? 0, migrationTimestamp);
        const remainingMs = eligibleAt - Date.now();
        const remainingHours = Math.max(0, Math.ceil(remainingMs / (60 * 60 * 1000)));
        return { type: 'pending', remainingHours };
      }
      default:
        return { type: 'pending', remainingHours: 0 };
    }
  }
  return snoozeUntil ? { type: 'snoozed' } : { type: 'ready' };
}

/** Format a display status for CLI output. */
export function formatDisplayStatus(status: DisplayStatus): string {
  switch (status.type) {
    case 'excluded': return 'excluded';
    case 'uploaded': return 'uploaded';
    case 'ready': return 'ready';
    case 'snoozed': return 'snoozed';
    case 'pending': return `pending (${status.remainingHours}h)`;
  }
}

/** Map display status type to a CLI color name. */
export function getDisplayStatusColor(status: DisplayStatus): 'green' | 'blue' | 'yellow' | 'default' {
  switch (status.type) {
    case 'ready': return 'green';
    case 'uploaded': return 'blue';
    case 'pending': case 'snoozed': return 'yellow';
    default: return 'default';
  }
}
