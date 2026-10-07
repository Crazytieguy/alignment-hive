import { appendFile, mkdir, mkdtemp, readFile, realpath, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import * as realAuth from '../lib/auth';
import * as realConvex from '../lib/convex';
import { mockForFile } from './file-mock';

// No network: heartbeats are recorded, and rejected for the ids in `rejecting`.
const sent: Array<{ sessionId: string; lineCount: number }> = [];
const rejecting = new Set<string>();
let root = '';
let transcripts = '';
let originalCwd = '';

mockForFile('../lib/convex', realConvex, {
  heartbeatSession: (session: { sessionId: string; lineCount: number }) => {
    if (rejecting.has(session.sessionId)) return Promise.reject(new Error('rejected'));
    sent.push(session);
    return Promise.resolve();
  },
});
mockForFile('../lib/auth', realAuth, { getAuthData: () => Promise.resolve({ accessToken: 'test' }) });

const { hiveHeartbeat } = await import('../commands/hive-heartbeat');
const { getStateDir, statePaths } = await import('../lib/config');
const { recordExcludedSession, recordUploadedSessions } = await import('../lib/session-state');

const OLD = new Date(Date.now() - 7 * 24 * 60 * 60 * 1000);
const transcriptLines = (count: number): string =>
  [
    JSON.stringify({ type: 'user', uuid: 'u0', parentUuid: null, cwd: root, message: { role: 'user', content: 'hi' } }),
    ...Array.from({ length: count - 1 }, (_, i) =>
      JSON.stringify({
        type: 'assistant',
        uuid: `a${i}`,
        parentUuid: 'u0',
        message: { role: 'assistant', content: 'ok' },
      }),
    ),
  ].join('\n') + '\n';

const sessionPath = (sessionId: string): string => join(transcripts, `${sessionId}.jsonl`);

async function writeSession(sessionId: string, lines: number): Promise<void> {
  await writeFile(sessionPath(sessionId), transcriptLines(lines));
  await utimes(sessionPath(sessionId), OLD, OLD);
}

const stateDir = (): string => getStateDir(root);
const sentIds = (): Array<string> => sent.map((s) => s.sessionId).sort();

beforeAll(async () => {
  originalCwd = process.cwd();
  root = await realpath(await mkdtemp(join(tmpdir(), 'hive-heartbeat-')));
  transcripts = join(root, 'transcripts');
  process.chdir(root);
});

afterAll(async () => {
  process.chdir(originalCwd);
  await rm(root, { recursive: true, force: true });
});

beforeEach(async () => {
  sent.length = 0;
  rejecting.clear();
  await rm(transcripts, { recursive: true, force: true });
  await mkdir(transcripts, { recursive: true });
  await rm(stateDir(), { recursive: true, force: true });
  await mkdir(stateDir(), { recursive: true });
  await writeFile(statePaths(stateDir()).transcriptsDirs, transcripts + '\n');
  await writeSession('pending', 2);
  await writeSession('uploaded', 3);
  await writeSession('excluded', 3);
  await recordUploadedSessions(stateDir(), [{ sessionId: 'uploaded', rawMtime: OLD.toISOString() }]);
  await recordExcludedSession(stateDir(), 'excluded');
  // An agent transcript: discovered, but heartbeats are for parent sessions only.
  await mkdir(join(transcripts, 'pending', 'subagents'), { recursive: true });
  await writeFile(join(transcripts, 'pending', 'subagents', 'agent-x.jsonl'), transcriptLines(2));
});

describe('heartbeat', () => {
  test('sends parent sessions that are neither uploaded nor excluded, on every run', async () => {
    expect(await hiveHeartbeat()).toBe(0);
    expect(sent.map((s) => [s.sessionId, s.lineCount])).toEqual([['pending', 2]]);

    sent.length = 0;
    expect(await hiveHeartbeat()).toBe(0);
    expect(sentIds()).toEqual(['pending']);
  });

  test('an uploaded session that grew after its upload is sent again', async () => {
    await appendFile(sessionPath('uploaded'), transcriptLines(1));
    expect(await hiveHeartbeat()).toBe(0);
    expect(sent.map((s) => [s.sessionId, s.lineCount]).sort()).toEqual([
      ['pending', 2],
      ['uploaded', 4],
    ]);
  });

  test('an excluded session is never sent, even after it changes', async () => {
    await appendFile(sessionPath('excluded'), transcriptLines(1));
    await hiveHeartbeat();
    expect(sentIds()).toEqual(['pending']);
  });

  test('a rejected heartbeat fails the run without stopping the others', async () => {
    await writeSession('pending2', 2);
    rejecting.add('pending');
    expect(await hiveHeartbeat()).toBe(1);
    expect(sentIds()).toEqual(['pending2']);
  });

  test('exits without scanning while another heartbeat holds the lock', async () => {
    const lock = statePaths(stateDir()).heartbeatLock;
    await writeFile(lock, String(process.pid)); // a live pid
    expect(await hiveHeartbeat()).toBe(0);
    expect(sent).toEqual([]);
    expect(await readFile(lock, 'utf-8')).toBe(String(process.pid));

    await rm(lock);
    expect(await hiveHeartbeat()).toBe(0);
    expect(sentIds()).toEqual(['pending']);
  });

  test('takes over a lock left by a dead process, and releases it when done', async () => {
    const lock = statePaths(stateDir()).heartbeatLock;
    await writeFile(lock, '2147483646'); // no such pid
    expect(await hiveHeartbeat()).toBe(0);
    expect(sentIds()).toEqual(['pending']);
    expect(await readFile(lock, 'utf-8').catch(() => null)).toBeNull();
  });
});
