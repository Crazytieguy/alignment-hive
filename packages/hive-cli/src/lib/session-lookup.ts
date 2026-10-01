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

/**
 * Resolve a prefix against parent sessions only, so agent ids never make a prefix ambiguous;
 * an exact agent id gets `agentError` instead of not-found.
 */
export function lookupParentSession(
  state: { parentSessions: Array<DiscoveredSession>; sessionById: Map<string, DiscoveredSession> },
  prefix: string,
  agentError: string,
): SessionLookupResult {
  const result = lookupRawSession(state.parentSessions, prefix);
  if (!result.found && state.sessionById.get(prefix)?.agentId) return { found: false, error: agentError };
  return result;
}
