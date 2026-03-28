import { describe, expect, test } from 'bun:test';
import {
  canExclude,
  canUpload,
  computeSessionStatus,
  formatSessionStatus,
  getStatusColor,
  isEligibleForAutoUpload,
} from '../lib/session-state';
import type { DiscoveredSession, StatusContext, UploadedEntry } from '../lib/session-state';

const DAY_MS = 24 * 60 * 60 * 1000;

function makeSession(overrides: Partial<DiscoveredSession> = {}): DiscoveredSession {
  return {
    sessionId: 'test-session-1',
    path: '/fake/path.jsonl',
    mtime: new Date(Date.now() - 2 * DAY_MS), // 2 days ago by default (past review period)
    ...overrides,
  };
}

function makeCtx(overrides: Partial<StatusContext> = {}): StatusContext {
  return {
    uploadedMap: new Map(),
    excludedSet: new Set(),
    consentMtime: Date.now() - 2 * DAY_MS, // 2 days ago by default (past review period)
    snoozeUntil: null,
    ...overrides,
  };
}

describe('computeSessionStatus', () => {
  test('returns excluded for sessions in excludedSet', () => {
    const session = makeSession();
    const ctx = makeCtx({ excludedSet: new Set(['test-session-1']) });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'excluded' });
  });

  test('returns uploaded when uploaded with same mtime and agentSessionIds', () => {
    const mtime = new Date(Date.now() - 2 * DAY_MS);
    const session = makeSession({ mtime });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: mtime.toISOString(),
      uploadedAt: new Date().toISOString(),
      agentSessionIds: [],
    };
    const ctx = makeCtx({ uploadedMap: new Map([['test-session-1', uploaded]]) });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'uploaded' });
  });

  test('returns uploaded for legacy upload without migration', () => {
    const mtime = new Date(Date.now() - 2 * DAY_MS);
    const session = makeSession({ mtime });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: mtime.toISOString(),
      uploadedAt: new Date().toISOString(),
      // no agentSessionIds — legacy
    };
    const ctx = makeCtx({
      uploadedMap: new Map([['test-session-1', uploaded]]),
      migrationTimestamp: null,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'uploaded' });
  });

  test('returns pending for legacy upload during migration review period', () => {
    const mtime = new Date(Date.now() - 2 * DAY_MS);
    const session = makeSession({ mtime });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: mtime.toISOString(),
      uploadedAt: new Date().toISOString(),
    };
    const migrationTimestamp = Date.now() - 1000; // just migrated
    const ctx = makeCtx({
      uploadedMap: new Map([['test-session-1', uploaded]]),
      migrationTimestamp,
    });
    const status = computeSessionStatus(session, ctx);
    expect(status.type).toBe('pending');
    if (status.type === 'pending') {
      expect(status.remainingMs).toBeGreaterThan(0);
      expect(status.remainingMs).toBeLessThanOrEqual(DAY_MS);
    }
  });

  test('returns ready for legacy upload after migration review period', () => {
    const mtime = new Date(Date.now() - 2 * DAY_MS);
    const session = makeSession({ mtime });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: mtime.toISOString(),
      uploadedAt: new Date().toISOString(),
    };
    const migrationTimestamp = Date.now() - 2 * DAY_MS; // migrated 2 days ago
    const ctx = makeCtx({
      uploadedMap: new Map([['test-session-1', uploaded]]),
      migrationTimestamp,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'ready' });
  });

  test('returns snoozed for legacy upload after migration when snoozed', () => {
    const mtime = new Date(Date.now() - 2 * DAY_MS);
    const session = makeSession({ mtime });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: mtime.toISOString(),
      uploadedAt: new Date().toISOString(),
    };
    const migrationTimestamp = Date.now() - 2 * DAY_MS;
    const ctx = makeCtx({
      uploadedMap: new Map([['test-session-1', uploaded]]),
      migrationTimestamp,
      snoozeUntil: Date.now() + DAY_MS,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'snoozed' });
  });

  test('returns ready for old session past all review periods', () => {
    const session = makeSession({ mtime: new Date(Date.now() - 3 * DAY_MS) });
    const ctx = makeCtx({ consentMtime: Date.now() - 3 * DAY_MS });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'ready' });
  });

  test('returns pending when session is within session review period', () => {
    const session = makeSession({ mtime: new Date(Date.now() - 1000) }); // just now
    const ctx = makeCtx({ consentMtime: Date.now() - 3 * DAY_MS });
    const status = computeSessionStatus(session, ctx);
    expect(status.type).toBe('pending');
  });

  test('returns pending when consent is within review period', () => {
    const session = makeSession({ mtime: new Date(Date.now() - 3 * DAY_MS) }); // old session
    const ctx = makeCtx({ consentMtime: Date.now() - 1000 }); // consent just given
    const status = computeSessionStatus(session, ctx);
    expect(status.type).toBe('pending');
  });

  test('returns snoozed for eligible session when snoozed', () => {
    const session = makeSession();
    const ctx = makeCtx({ snoozeUntil: Date.now() + DAY_MS });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'snoozed' });
  });

  test('excluded takes precedence over everything', () => {
    const mtime = new Date(Date.now() - 2 * DAY_MS);
    const session = makeSession({ mtime });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: mtime.toISOString(),
      uploadedAt: new Date().toISOString(),
      agentSessionIds: [],
    };
    const ctx = makeCtx({
      excludedSet: new Set(['test-session-1']),
      uploadedMap: new Map([['test-session-1', uploaded]]),
      snoozeUntil: Date.now() + DAY_MS,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'excluded' });
  });

  test('uploaded with changed mtime is treated as new session', () => {
    const session = makeSession({ mtime: new Date(Date.now() - 2 * DAY_MS) });
    const uploaded: UploadedEntry = {
      sessionId: 'test-session-1',
      rawMtime: new Date(Date.now() - 5 * DAY_MS).toISOString(), // different mtime
      uploadedAt: new Date().toISOString(),
      agentSessionIds: [],
    };
    const ctx = makeCtx({ uploadedMap: new Map([['test-session-1', uploaded]]) });
    // mtime changed → not considered uploaded, goes through normal eligibility
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'ready' });
  });
});

describe('status helper functions', () => {
  test('canExclude', () => {
    expect(canExclude({ type: 'ready' })).toBe(true);
    expect(canExclude({ type: 'pending', remainingMs: 1000 })).toBe(true);
    expect(canExclude({ type: 'snoozed' })).toBe(true);
    expect(canExclude({ type: 'excluded' })).toBe(false);
    expect(canExclude({ type: 'uploaded' })).toBe(false);
  });

  test('canUpload', () => {
    expect(canUpload({ type: 'ready' })).toBe(true);
    expect(canUpload({ type: 'pending', remainingMs: 1000 })).toBe(true);
    expect(canUpload({ type: 'snoozed' })).toBe(false);
    expect(canUpload({ type: 'excluded' })).toBe(false);
    expect(canUpload({ type: 'uploaded' })).toBe(false);
  });

  test('isEligibleForAutoUpload', () => {
    expect(isEligibleForAutoUpload({ type: 'ready' })).toBe(true);
    expect(isEligibleForAutoUpload({ type: 'pending', remainingMs: 1000 })).toBe(false);
    expect(isEligibleForAutoUpload({ type: 'snoozed' })).toBe(false);
    expect(isEligibleForAutoUpload({ type: 'excluded' })).toBe(false);
    expect(isEligibleForAutoUpload({ type: 'uploaded' })).toBe(false);
  });

  test('formatSessionStatus', () => {
    expect(formatSessionStatus({ type: 'ready' })).toBe('ready');
    expect(formatSessionStatus({ type: 'excluded' })).toBe('excluded');
    expect(formatSessionStatus({ type: 'uploaded' })).toBe('uploaded');
    expect(formatSessionStatus({ type: 'snoozed' })).toBe('snoozed');
    expect(formatSessionStatus({ type: 'pending', remainingMs: 2 * 60 * 60 * 1000 })).toBe('pending (2h)');
    expect(formatSessionStatus({ type: 'pending', remainingMs: 30 * 60 * 1000 })).toBe('pending (1h)');
    expect(formatSessionStatus({ type: 'pending', remainingMs: 0 })).toBe('pending (0h)');
  });

  test('getStatusColor', () => {
    expect(getStatusColor({ type: 'ready' })).toBe('green');
    expect(getStatusColor({ type: 'uploaded' })).toBe('blue');
    expect(getStatusColor({ type: 'pending', remainingMs: 1000 })).toBe('yellow');
    expect(getStatusColor({ type: 'snoozed' })).toBe('yellow');
    expect(getStatusColor({ type: 'excluded' })).toBe('default');
  });
});
