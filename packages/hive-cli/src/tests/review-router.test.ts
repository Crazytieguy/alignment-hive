import { mkdir, mkdtemp, realpath, rm, utimes, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { afterAll, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import * as realAuth from '../lib/auth';
import * as realConvex from '../lib/convex';
import { mockForFile } from './file-mock';

// No network: consent comes from `consent` and `history`, and every upload entry point throws
// unless a test captures it in `backend`.
let consent: () => Promise<{ consentMtime: number; ids: { directory: string } }>;
type History = { global: Array<{ sessionSharing: boolean; timestamp: number }>; project: History['global'] };
const sharingOn = (): History => ({
  global: [{ sessionSharing: true, timestamp: 1 }],
  project: [{ sessionSharing: true, timestamp: 1 }],
});
let history = sharingOn();
const blocked = (name: string) => () => {
  throw new Error(`${name} must not be called`);
};
const blockedBackend = () => ({
  generateUploadUrls: blocked('generateUploadUrls') as (...args: Array<any>) => unknown,
  saveUploads: blocked('saveUploads') as (...args: Array<any>) => unknown,
});
let backend = blockedBackend();
mockForFile('../lib/convex', realConvex, {
  resolveProjectConsent: () => consent(),
  getConsentHistory: () => Promise.resolve(history),
  generateUploadUrls: (...args: Array<any>) => backend.generateUploadUrls(...args),
  saveUploads: (...args: Array<any>) => backend.saveUploads(...args),
  saveWorkflowRuns: blocked('saveWorkflowRuns'),
});
mockForFile('../lib/auth', realAuth, { getAuthData: blocked('getAuthData') });

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
  history = sharingOn();
  backend = blockedBackend();
  await rm(statePaths(stateDir).excludedSessions, { force: true });
  await rm(statePaths(stateDir).sharingDisabled, { force: true });
  await rm(statePaths(stateDir).uploadLock, { force: true });
});

/** Capture uploads instead of sending them: the sent bodies, and the backend calls by name. */
function captureUploads() {
  const calls: Array<string> = [];
  const sent: Array<string> = [];
  backend = {
    generateUploadUrls: (parent: string, agents: Array<string>) => {
      calls.push('generateUploadUrls');
      return Object.fromEntries((agents.length ? agents : [parent]).map((id) => [id, `https://fake.invalid/${id}`]));
    },
    saveUploads: () => {
      calls.push('saveUploads');
    },
  };
  const realFetch = globalThis.fetch;
  globalThis.fetch = ((_url: unknown, init?: { body?: unknown }) => {
    sent.push(String(init?.body ?? ''));
    return Promise.resolve(new Response(JSON.stringify({ storageId: `st_${sent.length}` })));
  }) as unknown as typeof fetch;
  return { calls, sent, restore: () => (globalThis.fetch = realFetch) };
}

/** A transcript record of `sessionId`, as Claude Code writes it. */
const record = (sessionId: string, type: 'user' | 'assistant', uuid: string, parentUuid: string | null, text: string) =>
  JSON.stringify({
    type,
    uuid,
    parentUuid,
    sessionId,
    timestamp: OLD.toISOString(),
    cwd: root,
    message: { role: type, content: text },
  });

async function writeLines(path: string, records: Array<string>, mtime = OLD): Promise<void> {
  await writeFile(path, records.join('\n') + '\n');
  await utimes(path, mtime, mtime);
}

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

  test('refused while another upload holds the lock, without touching the backend', async () => {
    // The scheduled background send holds the lock (a live pid: this process).
    await writeFile(statePaths(stateDir).uploadLock, String(process.pid));
    const { calls, restore } = captureUploads();
    try {
      await expect(caller().sessions.upload({ sessionId: 'p2' })).rejects.toThrow(hive.upload.uploadInProgress);
      expect(calls).toEqual([]);
    } finally {
      restore();
    }
  });

  test('an already uploaded session succeeds with its fresh status', async () => {
    const r = await caller().sessions.upload({ sessionId: 'p3' });
    expect(r).toMatchObject({ ok: true, alreadyUploaded: true, status: { type: 'uploaded' } });
  });
});

describe('sharing was off when the session was last modified', () => {
  const S = 'not-shared-1';
  const path = () => join(transcripts, `${S}.jsonl`);
  beforeAll(async () => {
    await writeLines(path(), [record(S, 'user', 'n1', null, 'hi'), record(S, 'assistant', 'n2', 'n1', 'ok')]);
  });
  afterAll(async () => {
    await rm(path(), { force: true });
  });

  test('lists it as not-shared, not ready, and refuses its upload before any backend call', async () => {
    // Project sharing was turned off before the session's last change and on again after it.
    history = {
      global: [{ sessionSharing: true, timestamp: 1 }],
      project: [
        { sessionSharing: true, timestamp: 1 },
        { sessionSharing: false, timestamp: OLD.getTime() - DAY_MS },
        { sessionSharing: true, timestamp: OLD.getTime() + DAY_MS },
      ],
    };
    const { sessions } = await caller().sessions.list();
    expect(sessions.find((s) => s.sessionId === S)).toMatchObject({
      status: { type: 'not-shared' },
      statusLabel: 'sharing was off',
    });
    expect(sessions.find((s) => s.sessionId === 'p1')!.status).toEqual({ type: 'not-shared' });

    const { calls, restore } = captureUploads();
    try {
      await expect(caller().sessions.upload({ sessionId: S })).rejects.toThrow(hive.upload.outsideConsentWindow);
      expect(calls).toEqual([]);
    } finally {
      restore();
    }
  });
});

describe('a session that carries a copy of an excluded session', () => {
  // Resuming A copies A's records, under A's session id, into the new session B's file.
  const A = 'resumed-from-a';
  const B = 'resumed-as-b';
  const copyOfA = [
    record(A, 'user', 'c1', null, 'PRIVATE-A prompt'),
    record(A, 'assistant', 'c2', 'c1', 'PRIVATE-A reply'),
  ];
  beforeAll(async () => {
    await writeLines(join(transcripts, `${A}.jsonl`), copyOfA);
    await writeLines(join(transcripts, `${B}.jsonl`), [
      // A's title, whose leafUuid points into A's own file.
      JSON.stringify({ type: 'summary', summary: 'PRIVATE-A title', leafUuid: 'a-only' }),
      ...copyOfA,
      record(B, 'user', 'b1', 'c2', 'B follow-up'),
      record(B, 'assistant', 'b2', 'b1', 'B answer'),
    ]);
  });
  /** Back to the shared fixture's upload records: only p3 uploaded. */
  const resetUploads = async () => {
    await rm(statePaths(stateDir).startedUploads, { force: true });
    await writeFile(
      statePaths(stateDir).uploadedSessions,
      JSON.stringify({
        sessionId: 'p3',
        rawMtime: OLD.toISOString(),
        uploadedAt: new Date().toISOString(),
        agentSessionIds: [],
      }) + '\n',
    );
  };
  afterAll(async () => {
    await rm(join(transcripts, `${A}.jsonl`), { force: true });
    await rm(join(transcripts, `${B}.jsonl`), { force: true });
    await resetUploads();
  });

  test("once A is excluded, B neither shows nor uploads A's content, and keeps its own", async () => {
    const before = await caller().sessions.content({ sessionId: B });
    expect(JSON.stringify(before.entries)).toContain('PRIVATE-A');

    expect(await caller().sessions.exclude({ sessionId: A })).toMatchObject({ status: { type: 'excluded' } });

    const { sessions } = await caller().sessions.list();
    expect(sessions.find((s) => s.sessionId === B)!.summary).toBe('B follow-up');
    const content = await caller().sessions.content({ sessionId: B });
    expect(JSON.stringify(content.entries)).not.toContain('PRIVATE-A');
    expect(JSON.stringify(content.entries)).toContain('B answer');
    // A's records keep their place in the chain, so B's own records still hang off them.
    expect(content.entries.slice(0, 2)).toEqual([
      { type: 'user', uuid: 'c1', parentUuid: null },
      { type: 'assistant', uuid: 'c2', parentUuid: 'c1' },
    ]);
    // The excluded session's own preview still shows its content.
    expect(JSON.stringify((await caller().sessions.content({ sessionId: A })).entries)).toContain('PRIVATE-A');

    const { calls, sent, restore } = captureUploads();
    try {
      expect(await caller().sessions.upload({ sessionId: B })).toMatchObject({ ok: true });
      expect(calls).toEqual(['generateUploadUrls', 'saveUploads']);
      expect(sent.join('')).not.toContain('PRIVATE-A');
      expect(sent.join('')).toContain('B answer');
    } finally {
      restore();
    }
  });

  test('a session resumed from one that is not excluded keeps its copied title', async () => {
    const C = 'resumed-from-z';
    await writeLines(join(transcripts, `${C}.jsonl`), [
      JSON.stringify({ type: 'summary', summary: 'Z title', leafUuid: 'z-only' }),
      record('not-excluded-z', 'user', 'z1', null, 'Z prompt'),
      record(C, 'user', 'd1', 'z1', 'C follow-up'),
      record(C, 'assistant', 'd2', 'd1', 'C answer'),
    ]);
    try {
      await caller().sessions.exclude({ sessionId: A });
      const { sessions } = await caller().sessions.list();
      expect(sessions.find((s) => s.sessionId === C)!.summary).toBe('Z title');
      expect(JSON.stringify((await caller().sessions.content({ sessionId: C })).entries)).toContain('Z title');
    } finally {
      await rm(join(transcripts, `${C}.jsonl`), { force: true });
    }
  });

  test("excluding A while B's upload is under way stops it before A's copies are sent", async () => {
    await resetUploads();
    const { calls, sent, restore } = captureUploads();
    const mint = backend.generateUploadUrls;
    backend.generateUploadUrls = async (...args: Array<any>) => {
      expect(await caller().sessions.exclude({ sessionId: A })).toMatchObject({ status: { type: 'excluded' } });
      return mint(...args);
    };
    try {
      await expect(caller().sessions.upload({ sessionId: B })).rejects.toThrow(hive.upload.otherExcludedDuringUpload);
      expect(calls).toEqual(['generateUploadUrls']);
      expect(sent).toEqual([]);
    } finally {
      restore();
    }
    // The next upload reads B against the new excluded set.
    const retry = captureUploads();
    try {
      expect(await caller().sessions.upload({ sessionId: B })).toMatchObject({ ok: true });
      expect(retry.sent.join('')).not.toContain('PRIVATE-A');
      expect(retry.sent.join('')).toContain('B answer');
    } finally {
      retry.restore();
    }
  });
});
