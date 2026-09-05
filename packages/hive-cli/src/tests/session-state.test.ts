import { execSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import { statePaths } from '../lib/config';
import {
  MAX_RUN_UPLOAD_ATTEMPTS,
  computeSessionStatus,
  discoverSessions,
  excludeSessionChecked,
  hasIncompleteUpload,
  isSessionExcluded,
  loadSessionState,
  needsWorkflowReopen,
  runWorkflowBackfill,
} from '../lib/session-state';
import type { DiscoveredSession, SessionState, StatusContext, UploadedEntry } from '../lib/session-state';

const DAY_MS = 24 * 60 * 60 * 1000;

function makeSession(overrides: Partial<DiscoveredSession> = {}): DiscoveredSession {
  return {
    sessionId: 'test-session-1',
    path: '/fake/path.jsonl',
    mtime: new Date(Date.now() - 2 * DAY_MS), // past the review period by default
    ...overrides,
  };
}

function makeCtx(overrides: Partial<StatusContext> = {}): StatusContext {
  return {
    uploadedMap: new Map(),
    excludedSet: new Set(),
    consentMtime: Date.now() - 2 * DAY_MS, // past the review period by default
    snoozeUntil: null,
    ...overrides,
  };
}

/** An uploaded record for the session's current content. */
function uploadedFor(session: DiscoveredSession, overrides: Partial<UploadedEntry> = {}): UploadedEntry {
  return {
    sessionId: session.sessionId,
    rawMtime: session.mtime.toISOString(),
    uploadedAt: new Date().toISOString(),
    agentSessionIds: [],
    ...overrides,
  };
}

// Colliding sanitized dir names can put another project's transcripts in this
// project's directory — discoverSessions must not attribute those sessions here.
describe('discoverSessions', () => {
  let base: string;
  let projectRepo: string;
  let foreignRepo: string;
  let transcriptsDir: string;

  function writeSession(name: string, cwd: string | null, lines?: Array<object>): void {
    const entries = lines ?? [
      cwd ? { type: 'user', cwd, sessionId: name } : { type: 'user', sessionId: name },
      { type: 'assistant', message: { content: 'hi' } },
    ];
    writeFileSync(join(transcriptsDir, `${name}.jsonl`), entries.map((e) => JSON.stringify(e)).join('\n') + '\n');
  }

  beforeAll(() => {
    base = mkdtempSync(join(tmpdir(), 'hive-session-filter-'));
    projectRepo = join(base, 'proj.dot');
    foreignRepo = join(base, 'proj-dot');
    transcriptsDir = join(base, 'transcripts');
    for (const dir of [projectRepo, foreignRepo, transcriptsDir]) mkdirSync(dir);
    execSync('git init -q', { cwd: projectRepo });
    execSync('git init -q', { cwd: foreignRepo });

    writeSession('own-session', projectRepo);
    writeSession('foreign-session', foreignRepo);
    writeSession('deleted-cwd-session', join(base, 'gone', 'worktree'));
    writeSession('no-cwd-session', null);
    writeSession('user-only', projectRepo, [{ type: 'user', cwd: projectRepo, sessionId: 'user-only' }]);
    writeSession('agent-user-only', projectRepo, [{ type: 'user', cwd: projectRepo, sessionId: 'own-session' }]);
  });

  afterAll(() => {
    rmSync(base, { recursive: true, force: true });
  });

  test('drops sessions whose cwd resolves to a different project', async () => {
    const sessions = await discoverSessions([transcriptsDir], projectRepo);
    const ids = sessions.map((s) => s.sessionId).sort();
    expect(ids).toEqual(['agent-user-only', 'deleted-cwd-session', 'no-cwd-session', 'own-session']);
  });

  test('drops parent sessions with no assistant message but keeps agent files', async () => {
    const ids = (await discoverSessions([transcriptsDir], projectRepo)).map((s) => s.sessionId);
    expect(ids).not.toContain('user-only');
    expect(ids).toContain('agent-user-only');
  });
});

describe('loadSessionState', () => {
  test('rejects instead of treating an unreadable excluded-sessions file as empty', async () => {
    const stateDir = await mkdtemp(join(tmpdir(), 'hive-state-'));
    await mkdir(statePaths(stateDir).excludedSessions); // a directory, not a file
    await expect(loadSessionState(stateDir, [], stateDir)).rejects.toThrow();
    await rm(stateDir, { recursive: true, force: true });
  });
});

describe('computeSessionStatus', () => {
  test('returns excluded for sessions in excludedSet', () => {
    const session = makeSession();
    const ctx = makeCtx({ excludedSet: new Set([session.sessionId]) });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'excluded' });
  });

  test('returns uploaded when uploaded with same mtime and agentSessionIds', () => {
    const session = makeSession();
    const ctx = makeCtx({ uploadedMap: new Map([[session.sessionId, uploadedFor(session)]]) });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'uploaded' });
  });

  test('returns uploaded for a record without agentSessionIds when nothing was reopened', () => {
    const session = makeSession();
    const ctx = makeCtx({
      uploadedMap: new Map([[session.sessionId, uploadedFor(session, { agentSessionIds: undefined })]]),
      migrationTimestamp: null,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'uploaded' });
  });

  test('a reopened upload is pending during the reopen review period', () => {
    const session = makeSession();
    const ctx = makeCtx({
      uploadedMap: new Map([[session.sessionId, uploadedFor(session, { agentSessionIds: undefined })]]),
      migrationTimestamp: Date.now() - 1000, // just reopened
    });
    const status = computeSessionStatus(session, ctx);
    expect(status.type).toBe('pending');
    if (status.type === 'pending') {
      expect(status.remainingMs).toBeGreaterThan(0);
      expect(status.remainingMs).toBeLessThanOrEqual(DAY_MS);
    }
  });

  test('a reopened upload is ready after the reopen review period', () => {
    const session = makeSession();
    const ctx = makeCtx({
      uploadedMap: new Map([[session.sessionId, uploadedFor(session, { agentSessionIds: undefined })]]),
      migrationTimestamp: Date.now() - 2 * DAY_MS,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'ready' });
  });

  test('a reopened upload is snoozed after the review period when snoozed', () => {
    const session = makeSession();
    const ctx = makeCtx({
      uploadedMap: new Map([[session.sessionId, uploadedFor(session, { agentSessionIds: undefined })]]),
      migrationTimestamp: Date.now() - 2 * DAY_MS,
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
    const session = makeSession({ mtime: new Date(Date.now() - 1000) });
    const ctx = makeCtx({ consentMtime: Date.now() - 3 * DAY_MS });
    expect(computeSessionStatus(session, ctx).type).toBe('pending');
  });

  test('returns pending when consent is within review period', () => {
    const session = makeSession({ mtime: new Date(Date.now() - 3 * DAY_MS) });
    const ctx = makeCtx({ consentMtime: Date.now() - 1000 });
    expect(computeSessionStatus(session, ctx).type).toBe('pending');
  });

  test('returns snoozed for eligible session when snoozed', () => {
    const session = makeSession();
    const ctx = makeCtx({ snoozeUntil: Date.now() + DAY_MS });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'snoozed' });
  });

  test('excluded takes precedence over everything', () => {
    const session = makeSession();
    const ctx = makeCtx({
      excludedSet: new Set([session.sessionId]),
      uploadedMap: new Map([[session.sessionId, uploadedFor(session)]]),
      snoozeUntil: Date.now() + DAY_MS,
    });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'excluded' });
  });

  test('uploaded with changed mtime is treated as new session', () => {
    const session = makeSession();
    const stale = uploadedFor(session, { rawMtime: new Date(Date.now() - 5 * DAY_MS).toISOString() });
    const ctx = makeCtx({ uploadedMap: new Map([[session.sessionId, stale]]) });
    expect(computeSessionStatus(session, ctx)).toEqual({ type: 'ready' });
  });
});

describe('hasIncompleteUpload', () => {
  const uploadedEntry = (uploadedAt: string): UploadedEntry => ({
    sessionId: 's',
    rawMtime: 'm',
    uploadedAt,
    agentSessionIds: [],
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

  const session = (): DiscoveredSession => makeSession({ sessionId: 'sess-1' });
  const emptyState = () => ({
    uploadedMap: new Map<string, UploadedEntry>(),
    excludedSet: new Set<string>(),
    startedMap: new Map<string, number>(),
    migrationTimestamp: null,
  });

  test('records exclusion for an excludable session', async () => {
    const outcome = await excludeSessionChecked(stateDir, emptyState(), session());
    expect(outcome).toEqual({ result: 'excluded', hadPriorUpload: false });
    expect(await isSessionExcluded(stateDir, 'sess-1')).toBe(true);
  });

  test('refuses uploaded and partial sessions without writing', async () => {
    const uploadedState = { ...emptyState(), uploadedMap: new Map([['sess-1', uploadedFor(session())]]) };
    expect((await excludeSessionChecked(stateDir, uploadedState, session())).result).toBe('denied-uploaded');

    const partialState = { ...emptyState(), startedMap: new Map([['sess-1', Date.now()]]) };
    expect((await excludeSessionChecked(stateDir, partialState, session())).result).toBe('denied-partial');

    expect(await isSessionExcluded(stateDir, 'sess-1')).toBe(false);
  });

  test('reports already-excluded sessions', async () => {
    const state = { ...emptyState(), excludedSet: new Set(['sess-1']) };
    expect((await excludeSessionChecked(stateDir, state, session())).result).toBe('already-excluded');
  });

  test('excluding a reopened session succeeds but reports the prior upload', async () => {
    // A backfill reopen drops agentSessionIds in-memory; the entry (and the server data) remain.
    const state = {
      ...emptyState(),
      uploadedMap: new Map([['sess-1', uploadedFor(session(), { agentSessionIds: undefined })]]),
      migrationTimestamp: Date.now() - 2 * DAY_MS,
    };
    const outcome = await excludeSessionChecked(stateDir, state, session());
    expect(outcome).toEqual({ result: 'excluded', hadPriorUpload: true });
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

describe('needsWorkflowReopen (parse-gated — no run-id loop)', () => {
  test('reopens when a discovered agent is not in the recorded agentSessionIds', () => {
    const uploaded: UploadedEntry = { sessionId: 'p', rawMtime: 'm', uploadedAt: 't', agentSessionIds: ['agent-x'] };
    expect(needsWorkflowReopen(uploaded, [agentSession('agent-x'), agentSession('agent-y', 'wf_1')], [])).toBe(true);
  });

  test('reopens when a parseable run file is not in the recorded workflowRunIds', () => {
    const uploaded: UploadedEntry = {
      sessionId: 'p',
      rawMtime: 'm',
      uploadedAt: 't',
      agentSessionIds: ['agent-x', 'agent-y'],
      workflowRunIds: ['wf_1'],
    };
    expect(
      needsWorkflowReopen(uploaded, [agentSession('agent-x'), agentSession('agent-y', 'wf_1')], ['wf_1', 'wf_2']),
    ).toBe(true);
  });

  test('reopens a legacy upload (no workflowRunIds field) that has a parseable run file', () => {
    const uploaded: UploadedEntry = { sessionId: 'p', rawMtime: 'm', uploadedAt: 't', agentSessionIds: ['agent-x'] };
    expect(needsWorkflowReopen(uploaded, [agentSession('agent-x')], ['wf_1'])).toBe(true);
  });

  test('does NOT reopen when agents and parseable runs are all recorded — a malformed run file (absent from parseableRunIds) must not loop', () => {
    const uploaded: UploadedEntry = {
      sessionId: 'p',
      rawMtime: 'm',
      uploadedAt: 't',
      agentSessionIds: ['agent-x', 'agent-y'],
      workflowRunIds: [],
    };
    expect(needsWorkflowReopen(uploaded, [agentSession('agent-x'), agentSession('agent-y', 'wf_1')], [])).toBe(false);
  });

  test('does not reopen a parent with no agents and no runs', () => {
    const uploaded: UploadedEntry = { sessionId: 'p', rawMtime: 'm', uploadedAt: 't', agentSessionIds: [] };
    expect(needsWorkflowReopen(uploaded, [], [])).toBe(false);
  });

  test('reopens for a worktree-only run via recorded discoveredRunIds (invisible to parent-dir discovery)', () => {
    const uploaded: UploadedEntry = {
      sessionId: 'p',
      rawMtime: 'm',
      uploadedAt: 't',
      agentSessionIds: ['agent-x'],
      workflowRunIds: ['wf_1'],
      discoveredRunIds: ['wf_1', 'wf_worktree'],
    };
    expect(needsWorkflowReopen(uploaded, [agentSession('agent-x')], ['wf_1'])).toBe(true);
  });

  test('run-keyed reopen stops after MAX_RUN_UPLOAD_ATTEMPTS; agent-keyed reopen is unaffected', () => {
    const capped: UploadedEntry = {
      sessionId: 'p',
      rawMtime: 'm',
      uploadedAt: 't',
      agentSessionIds: ['agent-x'],
      workflowRunIds: [],
      runUploadAttempts: MAX_RUN_UPLOAD_ATTEMPTS,
    };
    expect(needsWorkflowReopen(capped, [agentSession('agent-x')], ['wf_stuck'])).toBe(false);
    // A missing agent still reopens even at the cap.
    expect(needsWorkflowReopen(capped, [agentSession('agent-x'), agentSession('agent-y')], ['wf_stuck'])).toBe(true);
    // Below the cap, the stuck run still retries.
    const belowCap: UploadedEntry = { ...capped, runUploadAttempts: MAX_RUN_UPLOAD_ATTEMPTS - 1 };
    expect(needsWorkflowReopen(belowCap, [agentSession('agent-x')], ['wf_stuck'])).toBe(true);
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
    };
  }

  const uploadedMissingAgent = (mtime: Date): UploadedEntry => ({
    sessionId: 'p',
    rawMtime: mtime.toISOString(),
    uploadedAt: 't',
    agentSessionIds: ['agent-x'],
  });
  const withWorkflowAgent = (): Array<DiscoveredSession> => [agentSession('agent-x'), agentSession('agent-y', 'wf_1')];
  const noRuns = (): Promise<Array<string>> => Promise.resolve([]);

  test('reopens an uploaded parent missing a workflow agent and routes it off the uploaded path', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const state = makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent());

    const ts = await runWorkflowBackfill(state, stateDir, noRuns);
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

  test('reuses the persisted window on a later run (stable; no reset each run)', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const first = await runWorkflowBackfill(
      makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent()),
      stateDir,
      noRuns,
    );
    const second = await runWorkflowBackfill(
      makeState(uploadedMissingAgent(mtime), mtime, withWorkflowAgent()),
      stateDir,
      noRuns,
    );
    expect(second).toBe(first);
  });

  test('leaves a fully-recorded upload untouched', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const uploaded: UploadedEntry = {
      sessionId: 'p',
      rawMtime: mtime.toISOString(),
      uploadedAt: 't',
      agentSessionIds: ['agent-x', 'agent-y'],
      workflowRunIds: ['wf_1'],
    };
    const state = makeState(uploaded, mtime, withWorkflowAgent());

    const ts = await runWorkflowBackfill(state, stateDir, () => Promise.resolve(['wf_1']));
    expect(ts).toBeNull();
    expect(state.uploadedMap.get('p')!.agentSessionIds).toEqual(['agent-x', 'agent-y']);
  });

  test('reopens when a parseable run file is missing from the recorded workflowRunIds', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const uploaded: UploadedEntry = {
      sessionId: 'p',
      rawMtime: mtime.toISOString(),
      uploadedAt: 't',
      agentSessionIds: ['agent-x', 'agent-y'],
      workflowRunIds: ['wf_1'],
    };
    const state = makeState(uploaded, mtime, withWorkflowAgent());

    const ts = await runWorkflowBackfill(state, stateDir, () => Promise.resolve(['wf_1', 'wf_2']));
    expect(typeof ts).toBe('number');
    expect(state.uploadedMap.get('p')!.agentSessionIds).toBeUndefined();
  });

  test('a run-discovery error is best-effort — no reopen, state loading unaffected', async () => {
    const mtime = new Date(Date.now() - 5 * DAY_MS);
    const uploaded: UploadedEntry = {
      sessionId: 'p',
      rawMtime: mtime.toISOString(),
      uploadedAt: 't',
      agentSessionIds: ['agent-x', 'agent-y'],
      workflowRunIds: [],
    };
    const state = makeState(uploaded, mtime, withWorkflowAgent());

    const ts = await runWorkflowBackfill(state, stateDir, () => Promise.reject(new Error('boom')));
    expect(ts).toBeNull();
    expect(state.uploadedMap.get('p')!.agentSessionIds).toEqual(['agent-x', 'agent-y']);
  });
});
