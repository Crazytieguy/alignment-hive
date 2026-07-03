/**
 * Upload status domain shared by the CLI and the review UI, so eligibility rules (especially
 * the exclusion veto — privacy-critical) have exactly one implementation.
 */

export type SessionStatus =
  | { type: 'excluded' }
  | { type: 'uploaded' }
  | { type: 'pending'; remainingMs: number }
  | { type: 'snoozed' }
  | { type: 'ready' };

/** States in which no complete upload of the current session content is recorded. */
function isPreUploadState(status: SessionStatus): boolean {
  return status.type === 'pending' || status.type === 'snoozed' || status.type === 'ready';
}

/**
 * Whether a session may be excluded from upload. Exclusion is a privacy veto — it must only
 * succeed when it can still prevent the data from reaching the backend. Uploaded sessions are
 * refused, and so are sessions whose most recent upload attempt started but never completed
 * (`hasPartialUpload`): part of their data may already be live on the server, where data
 * accessors could have downloaded it, so claiming they are excluded would be a lie.
 */
export function canExclude(status: SessionStatus, hasPartialUpload: boolean): boolean {
  return isPreUploadState(status) && !hasPartialUpload;
}

export function canUpload(status: SessionStatus): boolean {
  return status.type === 'ready' || status.type === 'pending';
}

export function isEligibleForAutoUpload(status: SessionStatus): boolean {
  return status.type === 'ready';
}

export function formatSessionStatus(status: SessionStatus, hasPartialUpload = false): string {
  // An incomplete upload attempt overrides the eligibility label: the user needs to know some
  // data is already on the server (and why Exclude is unavailable). Retry keeps following the
  // underlying status.
  if (hasPartialUpload && isPreUploadState(status)) {
    return 'partially uploaded';
  }
  switch (status.type) {
    case 'excluded': return 'excluded';
    case 'uploaded': return 'uploaded';
    case 'ready': return 'ready';
    case 'snoozed': return 'snoozed';
    case 'pending': {
      const remainingHours = Math.max(0, Math.ceil(status.remainingMs / (60 * 60 * 1000)));
      return `pending (${remainingHours}h)`;
    }
  }
}

export function getStatusColor(
  status: SessionStatus,
  hasPartialUpload = false,
): 'green' | 'blue' | 'yellow' | 'default' {
  if (hasPartialUpload && isPreUploadState(status)) return 'yellow';
  switch (status.type) {
    case 'ready': return 'green';
    case 'uploaded': return 'blue';
    case 'pending': case 'snoozed': return 'yellow';
    default: return 'default';
  }
}
