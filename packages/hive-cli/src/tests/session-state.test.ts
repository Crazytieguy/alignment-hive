import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  canExclude,
  canUpload,
  computeSessionStatus,
  excludeSessionChecked,
  formatSessionStatus,
  getStatusColor,
  hasIncompleteUpload,
  isEligibleForAutoUpload,
  needsWorkflowReopen,
  runWorkflowBackfill,
} from '../lib/session-state';
import type { DiscoveredSession, SessionState, StatusContext, UploadedEntry } from '../lib/session-state';

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
    expect(canExclude({ type: 'ready' }, false)).toBe(true);
    expect(canExclude({ type: 'pending', remainingMs: 1000 }, false)).toBe(true);
    expect(canExclude({ type: 'snoozed' }, false)).toBe(true);
    expect(canExclude({ type: 'excluded' }, false)).toBe(false);
    expect(canExclude({ type: 'uploaded' }, false)).toBe(false);
  });

  test('canExclude refuses any state with a partial upload (data may already be on the server)', () => {
    expect(canExclude({ type: 'ready' }, true)).toBe(false);
    expect(canExclude({ type: 'pending', remainingMs: 1000 }, true)).toBe(false);
    expect(canExclude({ type: 'snoozed' }, true)).toBe(false);
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

  test('formatSessionStatus surfaces a partial upload for pre-upload states only', () => {
    expect(formatSessionStatus({ type: 'ready' }, true)).toBe('partially uploaded');
    expect(formatSessionStatus({ type: 'pending', remainingMs: 1000 }, true)).toBe('partially uploaded');
    expect(formatSessionStatus({ type: 'snoozed' }, true)).toBe('partially uploaded');
    expect(formatSessionStatus({ type: 'uploaded' }, true)).toBe('uploaded');
    expect(formatSessionStatus({ type: 'excluded' }, true)).toBe('excluded');
  });

  test('getStatusColor', () => {
    expect(getStatusColor({ type: 'ready' })).toBe('green');
    expect(getStatusColor({ type: 'uploaded' })).toBe('blue');
    expect(getStatusColor({ type: 'pending', remainingMs: 1000 })).toBe('yellow');
    expect(getStatusColor({ type: 'snoozed' })).toBe('yellow');
    expect(getStatusColor({ type: 'excluded' })).toBe('default');
    expect(getStatusColor({ type: 'ready' }, true)).toBe('yellow');
  });
});

describe('hasIncompleteUpload', () => {
  const uploadedEntry = (uploadedAt: string): UploadedEntry => ({
    sessionId: 's', rawMtime: 'm', uploadedAt, agentSessionIds: [],
  });

  test('false with no started marker', () => {
    expect(hasIncompleteUpload('s', new Map(), new Map())).toBe(false);
  });

  test('true when an attempt started but nothing completed', () => {
    expect(hasIncompleteUpload('s', new Map(), new Map([['s', Date.now()]]))).toBe(true);
  });

  test('false when the completed record is newer than the attempt', () => {
    const uploaded = new Map([['s', uploadedEntry(new Date().toISOString())]]);
    const started = new Map([['s', Date.now() - 60_000]]);
    expect(hasIncompleteUpload('s', uploaded, started)).toBe(false);
  });

  test('true when a newer attempt started after the last completed upload', () => {
    const uploaded = new Map([['s', uploadedEntry(new Date(Date.now() - 60_000).toISOString())]]);
    const started = new Map([['s', Date.now()]]);
    expect(hasIncompleteUpload('s', uploaded, started)).toBe(true);
  });

  test('fails closed on an unparseable completion timestamp', () => {
    const uploaded = new Map([['s', uploadedEntry('not-a-date')]]);
    const started = new Map([['s', Date.now()]]);
    expect(hasIncompleteUpload('s', uploaded, started)).toBe(true);
  });
});

describe('excludeSessionChecked', () => {
  let stateDir: string;
  beforeEach(async () => {
    stateDir = await mkdtemp(join(tmpdir(), 'hive-excl-'));
  });
  afterEach(async () => {
    await rm(stateDir, { recursive: true, force: true });
  });

  test('records exclusion for an excludable session', async () => {
    expect(await excludeSessionChecked(stateDir, 'sess-1', { type: 'ready' }, false)).toBe('excluded');
    const content = await readFile(join(stateDir, 'excluded-sessions'), 'utf-8');
    expect(content).toBe('sess-1\n');
  });

  test('refuses uploaded and partial sessions without writing', async () => {
    expect(await excludeSessionChecked(stateDir, 'sess-1', { type: 'uploaded' }, false)).toBe('denied-uploaded');
    expect(await excludeSessionChecked(stateDir, 'sess-1', { type: 'ready' }, true)).toBe('denied-partial');
    expect(readFile(join(stateDir, 'excluded-sessions'), 'utf-8')).rejects.toThrow();
  });

  test('reports already-excluded sessions', async () => {
    expect(await excludeSessionChecked(stateDir, 'sess-1', { type: 'excluded' }, false)).toBe('already-excluded');
  });
});

function agentSession(sessionId: string, workflowRunId?: string): DiscoveredSession {
  return {
    sessionId,
    path: `/fake/${sessionId}.jsonl`,
    mtime: new Date(),
    agentId: sessionId.replace('agent-', ''),
    ...(workflowRunId && { workflowRunId }),
  };
}

describe('needsWorkflowReopen (agent-only — no run-id loop)', () => {
  test('reopens when a discovered agent is not in the recorded agentSessionIds', () => {
    const uploaded: UploadedEntry = { sessionId: 'p', rawMtime: 'm', uploadedAt: 't', agentSessionIds: ['agent-x'] };
    expect(needsWorkflowReopen(uploaded, [agentSession('agent-x'), agentSession('agent-y', 'wf_1')])).toBe(true);
  });

  test('does NOT reopen when all agents are recorded even if a run id is missing (unrecordable run metadata must not loop)', () => {
    const uploaded: UploadedEntry = { sessionId: 'p', rawMtime: 'm', uploadedAt: 't', agentSessionIds: ['agent-x', 'agent-y'], workflowRunIds: [] };
    expect(needsWorkflowReopen(uploaded, [agentSession('agent-x'), agentSession('agent-y', 'wf_1')])).toBe(false);
  });

  test('does not reopen a parent with no agents', () => {
    const uploaded: UploadedEntry = { sessionId: 'p', rawMtime: 'm', uploadedAt: 't', agentSessionIds: [] };
    expect(needsWorkflowReopen(uploaded, [])).toBe(false);
  });
});

describe('runWorkflowBackfill', () => {
  let stateDir: string;
  beforeEach(async () => {
    stateDir = await mkdtemp(join(tmpdir(), 'hive-h-'));
  });
  afterEach(async () => {
    await rm(stateDir, { recursive: true, force: true });
  });

  function makeState(uploaded: UploadedEntry, parentMtime: Date, agents: Array<DiscoveredSession>): SessionState {
    const parent: DiscoveredSession = { sessionId: 'p', path: '/fake/p.jsonl', mtime: parentMtime };
    return {
      parentSessions: [parent],
      agentsByParent: new Map([['p', agents]]),
      sessionById: new Map(),
      uploadedMap: new Map([['p', uploaded]]),
      excludedSet: new Set(),
      startedMap: new Map(),
      migrationTimestamp: null,
    };
  }

  const uploadedMissingAgent = (mtime: Date): UploadedEntry => ({
    sessionId: 'p', rawMtime: mtime.toISOString(), uploadedAt: 't', agentSessionIds: ['agent-x'],
  });
  const withWorkflowAgent = (): Array<DiscoveredSession> => [agentSession('agent-x'), agentSession('agent-y', 'wf_1')];

  test('reopens an uploaded parent missing a workflow agent and routes it off the uploaded path', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const state = makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent());

    const ts = await runWorkflowBackfill(state, stateDir, null);
    expect(typeof ts).toBe('number');
    expect(state.uploadedMap.get('p')!.agentSessionIds).toBeUndefined();

    const status = computeSessionStatus(state.parentSessions[0], {
      uploadedMap: state.uploadedMap,
      excludedSet: state.excludedSet,
      consentMtime: Date.now() - 5 * DAY_MS,
      snoozeUntil: null,
      migrationTimestamp: ts,
    });
    expect(status.type).toBe('pending');
  });

  test('writes a FRESH review window even when a stale agent-migration timestamp exists', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const state = makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent());
    const staleAgentTs = Date.now() - 30 * DAY_MS; // long-expired agent-migration window

    const ts = await runWorkflowBackfill(state, stateDir, staleAgentTs);
    expect(ts).toBeGreaterThan(Date.now() - DAY_MS); // fresh, not the stale agent ts

    const status = computeSessionStatus(state.parentSessions[0], {
      uploadedMap: state.uploadedMap,
      excludedSet: state.excludedSet,
      consentMtime: Date.now() - 5 * DAY_MS,
      snoozeUntil: null,
      migrationTimestamp: ts,
    });
    expect(status.type).toBe('pending'); // within the fresh window, NOT immediately ready
  });

  test('reuses the persisted window on a later run (stable; no reset each run)', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const first = await runWorkflowBackfill(makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent()), stateDir, null);
    const second = await runWorkflowBackfill(makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent()), stateDir, null);
    expect(second).toBe(first);
  });

  test('leaves a fully-recorded upload untouched', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const uploaded: UploadedEntry = {
      sessionId: 'p', rawMtime: mtime.toISOString(), uploadedAt: 't',
      agentSessionIds: ['agent-x', 'agent-y'], workflowRunIds: ['wf_1'],
    };
    const state = makeState(uploaded, mtime, withWorkflowAgent());

    const ts = await runWorkflowBackfill(state, stateDir, null);
    expect(ts).toBeNull();
    expect(state.uploadedMap.get('p')!.agentSessionIds).toEqual(['agent-x', 'agent-y']);
  });
});
