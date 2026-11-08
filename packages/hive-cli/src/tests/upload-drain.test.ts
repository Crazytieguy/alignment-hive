// An upload whose batch has one failed transfer reports back only after its other transfers in
// that batch settle: `hive upload send` exits as soon as the result comes back, and the review UI
// reports it as done.

import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test';
import * as realConvex from '../lib/convex';
import { mockForFile } from './file-mock';
import type { DiscoveredSession } from '../lib/session-state';

const SID = 'drain-parent';
const saved: Array<string> = [];
mockForFile('../lib/convex', realConvex, {
  generateUploadUrls: (sessionId: string, agentIds: Array<string>, _ids: unknown, runIds: Array<string> = []) =>
    Promise.resolve(Object.fromEntries([sessionId, ...agentIds, ...runIds].map((id) => [id, `https://blob.test/${id}`]))),
  saveUploads: (_parent: string, _meta: unknown, uploads: Array<{ sessionId: string }>) => {
    saved.push(...uploads.map((u) => u.sessionId));
    return Promise.resolve();
  },
  saveWorkflowRuns: (_parent: string, _meta: unknown, runs: Array<{ workflowRunId: string }>) => {
    saved.push(...runs.map((r) => r.workflowRunId));
    return Promise.resolve();
  },
});

const { uploadOneSession } = await import('../lib/upload-session');
const { loadUploadedSessions } = await import('../lib/session-state');

/** A transfer the test settles by hand, and a promise of its request being made. */
function heldTransfer() {
  let settle!: (response: Response) => void;
  let requested!: () => void;
  const response = new Promise<Response>((resolve) => (settle = resolve));
  const made = new Promise<void>((resolve) => (requested = resolve));
  return { settle, made, request: () => (requested(), response) };
}

const ok = (id: string) => Response.json({ storageId: `storage-${id}` });
// Without the drain the upload resolves a few ms after the failure (the run path still writes its
// local record first), so this window is generous enough for a loaded machine. It only bounds how
// long the test waits; with the drain the upload stays pending however long it is.
const SETTLE_WINDOW_MS = 250;

let root = '';
let fetchSpy: ReturnType<typeof spyOn<typeof globalThis, 'fetch'>>;
let held: Map<string, ReturnType<typeof heldTransfer>>;

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'hive-drain-'));
  saved.length = 0;
  held = new Map();
  fetchSpy = spyOn(globalThis, 'fetch').mockImplementation(((url: string) => {
    const id = url.slice('https://blob.test/'.length);
    return held.get(id)?.request() ?? Promise.resolve(ok(id));
  }) as typeof fetch);
});

afterEach(async () => {
  fetchSpy.mockRestore();
  await rm(root, { recursive: true, force: true });
});

const line = (sessionId: string) =>
  JSON.stringify({
    type: 'user',
    uuid: `u-${sessionId}`,
    parentUuid: null,
    sessionId: SID,
    timestamp: '2026-01-01T00:00:00Z',
    message: { role: 'user', content: 'hi' },
  });

async function session(sessionId: string, extra: Partial<DiscoveredSession> = {}): Promise<DiscoveredSession> {
  const path = join(root, `${sessionId}.jsonl`);
  await writeFile(path, line(sessionId) + '\n');
  return { sessionId, path, mtime: new Date(), ...extra };
}

async function upload(agents: Array<DiscoveredSession>) {
  const stateDir = join(root, 'state');
  await mkdir(stateDir, { recursive: true });
  let done = false;
  const result = uploadOneSession({
    session: await session(SID),
    state: { agentsByParent: new Map([[SID, agents]]) },
    statusCtx: {
      uploadedMap: new Map(),
      excludedSet: new Set(),
      consentMtime: 0,
      snoozeUntil: null,
      consentWindows: { global: [{ start: 0, end: Infinity }], project: [{ start: 0, end: Infinity }] },
    },
    transcriptsDirs: [],
    checkoutId: 'c',
    ids: { directory: '/proj' },
    stateDir,
  }).finally(() => (done = true));
  return { result, done: () => done, stateDir };
}

/** Fail `failing` once `pending`'s request is made, and check the upload is still waiting on it. */
async function failOneWhileOtherInFlight(failing: string, pending: string, isDone: () => boolean) {
  await Promise.all([held.get(failing)!.made, held.get(pending)!.made]);
  held.get(failing)!.settle(new Response('boom', { status: 500 }));
  await new Promise((resolve) => setTimeout(resolve, SETTLE_WINDOW_MS));
  expect(isDone()).toBe(false);
  held.get(pending)!.settle(ok(pending));
}

test('a failed agent transfer waits for its sibling, and saves neither the batch nor the parent as done', async () => {
  held.set('agent-a', heldTransfer());
  held.set('agent-b', heldTransfer());
  const agents = [
    await session('agent-a', { parentSessionId: SID }),
    await session('agent-b', { parentSessionId: SID }),
  ];
  const run = await upload(agents);
  await failOneWhileOtherInFlight('agent-a', 'agent-b', run.done);
  expect(await run.result).toEqual({ ok: false, error: 'Upload failed: 500' });
  expect(saved).toEqual([SID]);
  expect((await loadUploadedSessions(run.stateDir)).has(SID)).toBe(false);
});

test('a failed workflow-run transfer waits for its sibling, then records the attempt as before', async () => {
  const workflows = join(root, SID, 'workflows');
  await mkdir(workflows, { recursive: true });
  for (const id of ['wf_one', 'wf_two']) await writeFile(join(workflows, `${id}.json`), JSON.stringify({ runId: id }));
  held.set('wf_one', heldTransfer());
  held.set('wf_two', heldTransfer());
  const run = await upload([]);
  await failOneWhileOtherInFlight('wf_one', 'wf_two', run.done);
  expect(await run.result).toEqual({ ok: true, agentCount: 0 });
  expect(saved).toEqual([SID]);
  const entry = (await loadUploadedSessions(run.stateDir)).get(SID);
  expect(entry).toMatchObject({ workflowRunIds: [], runUploadAttempts: 1 });
  expect(entry!.discoveredRunIds!.toSorted()).toEqual(['wf_one', 'wf_two']);
});
