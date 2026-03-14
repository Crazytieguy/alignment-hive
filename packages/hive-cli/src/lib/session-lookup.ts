import type { DiscoveredSession } from './session-state';

export type SessionLookupResult =
  | { found: true; session: DiscoveredSession }
  | { found: false; error: string; matches?: Array<DiscoveredSession> };

/** Look up a session by ID prefix from discovered sessions. */
export function lookupRawSession(
  sessions: Array<DiscoveredSession>,
  prefix: string,
): SessionLookupResult {
  const matches = sessions.filter((s) => s.sessionId.startsWith(prefix));

  if (matches.length === 0) {
    return { found: false, error: `No session matching "${prefix}"` };
  }

  if (matches.length === 1) {
    return { found: true, session: matches[0] };
  }

  return {
    found: false,
    error: `Multiple sessions match "${prefix}":`,
    matches: matches.slice(0, 10),
  };
}
