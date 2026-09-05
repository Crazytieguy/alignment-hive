import { errors } from './messages';
import type { DiscoveredSession } from './session-state';

const MAX_LISTED_MATCHES = 5;

export type SessionLookupResult = { found: true; session: DiscoveredSession } | { found: false; error: string };

/** Resolve a session id prefix against discovered sessions; the error text lists ambiguous matches. */
export function lookupRawSession(sessions: Array<DiscoveredSession>, prefix: string): SessionLookupResult {
  const matches = sessions.filter((s) => s.sessionId.startsWith(prefix));
  if (matches.length === 1) return { found: true, session: matches[0] };
  if (matches.length === 0) return { found: false, error: errors.sessionNotFound(prefix) };
  const shown = matches.slice(0, MAX_LISTED_MATCHES).map((m) => `  ${m.sessionId.slice(0, 16)}`);
  if (matches.length > MAX_LISTED_MATCHES) shown.push(errors.andMore(matches.length - MAX_LISTED_MATCHES));
  return { found: false, error: [errors.multipleSessions(prefix), ...shown].join('\n') };
}

/** Shortest prefix (at least minLen chars) that identifies each id uniquely among the given ids. */
export function computeMinimalPrefixes(ids: Array<string>, minLen = 4): Map<string, string> {
  const result = new Map<string, string>();
  for (const id of ids) {
    let len = minLen;
    while (len < id.length && ids.some((other) => other !== id && other.startsWith(id.slice(0, len)))) len++;
    result.set(id, id.slice(0, len));
  }
  return result;
}
