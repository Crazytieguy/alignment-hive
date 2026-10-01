import { mkdir, mkdtemp, realpath, rm, utimes, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import * as realConvex from '../lib/convex';

// No network: consent comes from `consent`, and every upload entry point throws.
let consent: () => Promise<{ consentMtime: number; ids: { directory: string } }>;
const blocked = (name: string) => () => {
  throw new Error(`${name} must not be called`);
};
mock.module('../lib/convex', () => ({
  ...realConvex,
  resolveProjectConsent: () => consent(),
  getConsentHistory: () => Promise.resolve({ global: [], project: [] }),
  generateUploadUrls: blocked('generateUploadUrls'),
  saveUploads: blocked('saveUploads'),
  saveWorkflowRuns: blocked('saveWorkflowRuns'),
}));
mock.module('../lib/auth', () => ({ getAuthData: blocked('getAuthData') }));

const { createReviewRouter } = await import('../lib/review-router');
const { statePaths } = await import('../lib/config');
const { hive } = await import('../lib/messages');

const DAY_MS = 24 * 60 * 60 * 1000;
const TOKEN = 'ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs';
const OLD = new Date(Date.now() - 3 * DAY_MS);

let root = '';
let transcripts = '';
let stateDir = '';

const lines = (cwd: string, text: string) =>
  [
    {
      type: 'user',
      uuid: 'u1',
      parentUuid: null,
      timestamp: OLD.toISOString(),
      cwd,
      message: { role: 'user', content: text },
    },
    {
      type: 'assistant',
      uuid: 'a1',
      parentUuid: 'u1',
      timestamp: OLD.toISOString(),
      message: { role: 'assistant', content: 'ok' },
    },
  ]
    .map((l) => JSON.stringify(l))
    .join('\n') + '\n';

async function writeTranscript(path: string, text: string): Promise<void> {
  await mkdir(join(path, '..'), { recursive: true });
  await writeFile(path, lines(root, text));
  await utimes(path, OLD, OLD);
}

const caller = () => createReviewRouter(stateDir, root).createCaller({});

beforeAll(async () => {
  root = await realpath(await mkdtemp(join(tmpdir(), 'hive-review-router-')));
  transcripts = join(root, 'transcripts');
  stateDir = join(root, 'state');
  await mkdir(stateDir, { recursive: true });
  await writeFile(statePaths(stateDir).transcriptsDirs, transcripts + '\n');

  await writeTranscript(join(transcripts, 'p1.jsonl'), 'Fix the bug');
  await writeTranscript(join(transcripts, 'p1', 'subagents', 'agent-a1.jsonl'), `inspect ${TOKEN}`);
  await writeTranscript(join(transcripts, 'p1', 'subagents', 'workflows', 'wf_r1', 'agent-w1.jsonl'), 'step');
  await mkdir(join(transcripts, 'p1', 'workflows'), { recursive: true });
  await writeFile(
    join(transcripts, 'p1', 'workflows', 'wf_r1.json'),
    JSON.stringify({ runId: 'wf_r1', workflowName: 'review', result: TOKEN }),
  );
  await writeTranscript(join(transcripts, 'p2.jsonl'), 'Second');
  await writeTranscript(join(transcripts, 'p3.jsonl'), 'Already up');
  await writeFile(
    statePaths(stateDir).uploadedSessions,
    JSON.stringify({
      sessionId: 'p3',
      rawMtime: OLD.toISOString(),
      uploadedAt: new Date().toISOString(),
      agentSessionIds: [],
    }) + '\n',
  );
});

afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

beforeEach(async () => {
  consent = () => Promise.resolve({ consentMtime: 0, ids: { directory: root } });
  await rm(statePaths(stateDir).excludedSessions, { force: true });
  await rm(statePaths(stateDir).sharingDisabled, { force: true });
});

describe('sessions.list', () => {
  test('lists parents with statuses and summaries', async () => {
    const { sessions, consentError } = await caller().sessions.list();
    expect(consentError).toBeUndefined();
    const byId = new Map(sessions.map((s) => [s.sessionId, s]));
    expect([...byId.keys()].sort()).toEqual(['p1', 'p2', 'p3']);
    expect(byId.get('p1')).toMatchObject({ status: { type: 'ready' }, summary: 'Fix the bug' });
    expect(byId.get('p3')!.status).toEqual({ type: 'uploaded' });
  });

  test('without consent, still lists and shows nothing as more uploadable than pending', async () => {
    consent = () => Promise.reject(new Error('no consent'));
    const { sessions, consentError } = await caller().sessions.list();
    expect(consentError).toBe('no consent');
    const types = Object.fromEntries(sessions.map((s) => [s.sessionId, s.status.type]));
    expect(types).toEqual({ p1: 'pending', p2: 'pending', p3: 'uploaded' });
  });
});

describe('sessions.content', () => {
  test('returns the parent entries, status, and agents and runs as metadata only', async () => {
    const content = await caller().sessions.content({ sessionId: 'p1' });
    expect(content.entries.length).toBeGreaterThan(0);
    expect(content.status).toEqual({ type: 'ready' });
    expect(content.partialUpload).toBe(false);
    expect(content.agents.sort((a, b) => a.agentId.localeCompare(b.agentId))).toEqual([
      { sessionId: 'agent-a1', agentId: 'a1' },
      { sessionId: 'agent-w1', agentId: 'w1', workflowRunId: 'wf_r1' },
    ]);
    expect(content.workflowRuns).toEqual([
      expect.objectContaining({ runId: 'wf_r1', workflowRunId: 'wf_r1', workflowName: 'review' }),
    ]);
    expect(content.workflowRuns[0]).not.toHaveProperty('blob');
  });

  test('agentContent and workflowRun return one sanitized agent or run', async () => {
    const agent = await caller().sessions.agentContent({ sessionId: 'p1', agentId: 'a1' });
    expect(agent.messageCount).toBe(agent.entries.length);
    expect(JSON.stringify(agent.entries)).toContain('inspect [REDACTED:');
    expect(JSON.stringify(agent.entries)).not.toContain(TOKEN);

    const { blob } = await caller().sessions.workflowRun({ sessionId: 'p1', runId: 'wf_r1' });
    expect(blob).toMatchObject({ runId: 'wf_r1', workflowName: 'review' });
    expect(JSON.stringify(blob)).not.toContain(TOKEN);

    await expect(caller().sessions.agentContent({ sessionId: 'p1', agentId: 'nope' })).rejects.toThrow();
    await expect(caller().sessions.workflowRun({ sessionId: 'p1', runId: 'wf_nope' })).rejects.toThrow();
  });
});

describe('sessions.exclude / excludeMany', () => {
  test('exclude returns the fresh status', async () => {
    const r = await caller().sessions.exclude({ sessionId: 'p1' });
    expect(r).toMatchObject({ alreadyExcluded: false, hadPriorUpload: false, status: { type: 'excluded' } });
    expect((await caller().sessions.exclude({ sessionId: 'p1' })).alreadyExcluded).toBe(true);
  });

  test('excludeMany attempts every id; one failure does not stop the rest', async () => {
    const { results } = await caller().sessions.excludeMany({ sessionIds: ['p3', 'missing', 'agent-a1', 'p2'] });
    expect(results.map((r) => [r.sessionId, r.ok])).toEqual([
      ['p3', false],
      ['missing', false],
      ['agent-a1', false],
      ['p2', true],
    ]);
    expect(results[0].error).toBe(hive.upload.cannotExcludeUploaded('p3'));
    expect(results[2].error).toBe(hive.upload.agentCannotExclude);
    expect(results[3]).toMatchObject({ status: { type: 'excluded' }, alreadyExcluded: false });
  });
});

describe('sessions.upload', () => {
  test('an upload that does not happen throws its reason', async () => {
    await writeFile(statePaths(stateDir).sharingDisabled, '');
    await expect(caller().sessions.upload({ sessionId: 'p1' })).rejects.toThrow(hive.upload.noProjectConsent);
  });

  test('an already uploaded session succeeds with its fresh status', async () => {
    const r = await caller().sessions.upload({ sessionId: 'p3' });
    expect(r).toMatchObject({ ok: true, alreadyUploaded: true, status: { type: 'uploaded' } });
  });
});
