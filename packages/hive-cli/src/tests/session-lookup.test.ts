import { describe, expect, test } from 'bun:test';
import { lookupRawSession } from '../lib/session-lookup';
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
