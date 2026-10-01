import { describe, expect, test } from 'bun:test';
import { lookupParentSession, lookupRawSession } from '../lib/session-lookup';
import type { DiscoveredSession } from '../lib/session-state';

const s = (id: string): DiscoveredSession => ({ sessionId: id, path: `/x/${id}.jsonl`, mtime: new Date() });

describe('lookupRawSession', () => {
  test('refuses an ambiguous prefix, resolves a unique one, and reports no match', () => {
    const sessions = [s('abc-1'), s('abd-2')];
    const ambiguous = lookupRawSession(sessions, 'ab');
    expect(ambiguous.found).toBe(false);
    if (!ambiguous.found) expect(ambiguous.error).toContain('abd-2');
    expect(lookupRawSession(sessions, 'abc')).toEqual({ found: true, session: sessions[0] });
    expect(lookupRawSession(sessions, 'zzz').found).toBe(false);
  });
});

describe('lookupParentSession', () => {
  test('agent ids never make a prefix ambiguous; an exact agent id gets the agent error', () => {
    const parent = s('abc-1');
    const agent = { ...s('abd-agent'), agentId: 'abd-agent' };
    const state = { parentSessions: [parent], sessionById: new Map([parent, agent].map((x) => [x.sessionId, x])) };
    expect(lookupParentSession(state, 'ab', 'agent')).toEqual({ found: true, session: parent });
    expect(lookupParentSession(state, 'abd-agent', 'agent')).toEqual({ found: false, error: 'agent' });
    expect(lookupParentSession(state, 'abd', 'agent').found).toBe(false);
  });
});
