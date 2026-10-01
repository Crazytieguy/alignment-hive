import { mkdir, mkdtemp, realpath, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import * as realAuth from '../lib/auth';
import * as realConvex from '../lib/convex';
import * as realHookInput from '../lib/hook-input';
import * as realSpawn from '../lib/spawn';
import { mockForFile } from './file-mock';

// No network and no processes: consent comes from `history`, and spawns are only recorded.
const DAY_MS = 24 * 60 * 60 * 1000;
const OLD = new Date(Date.now() - 7 * DAY_MS);
let history: { global: Array<{ sessionSharing: boolean; timestamp: number }>; project: typeof history.global };
const spawned: Array<Array<string>> = [];
let root = '';

mockForFile('../lib/convex', realConvex, {
  resolveProjectConsent: (cwd: string) => Promise.resolve({ consentMtime: 0, ids: { directory: cwd } }),
  getConsentHistory: () => Promise.resolve(history),
});
mockForFile('../lib/auth', realAuth, { getAuthData: () => Promise.resolve({ accessToken: 'test' }) });
mockForFile('../lib/hook-input', realHookInput, { readHookInput: () => Promise.resolve({ cwd: root }) });
mockForFile('../lib/spawn', realSpawn, {
  spawnBackgroundCommand: (args: Array<string>) => {
    spawned.push(args);
    return true;
  },
});

const { hiveSessionStart } = await import('../commands/hive-session-start');
const { getStateDir, statePaths } = await import('../lib/config');

beforeAll(async () => {
  root = await realpath(await mkdtemp(join(tmpdir(), 'hive-session-start-')));
  const transcripts = join(root, 'transcripts');
  await mkdir(transcripts, { recursive: true });
  const path = join(transcripts, 's1.jsonl');
  await writeFile(
    path,
    [
      { type: 'user', uuid: 'u1', parentUuid: null, cwd: root, message: { role: 'user', content: 'hi' } },
      { type: 'assistant', uuid: 'a1', parentUuid: 'u1', message: { role: 'assistant', content: 'ok' } },
    ]
      .map((r) => JSON.stringify(r))
      .join('\n') + '\n',
  );
  await utimes(path, OLD, OLD);
});

afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

beforeEach(async () => {
  spawned.length = 0;
  const stateDir = getStateDir(root);
  await rm(stateDir, { recursive: true, force: true });
  await mkdir(stateDir, { recursive: true });
  await writeFile(statePaths(stateDir).transcriptsDirs, join(root, 'transcripts') + '\n');
});

async function run(): Promise<string> {
  const log = spyOn(console, 'log').mockImplementation(() => {});
  try {
    await hiveSessionStart();
    return log.mock.calls.map((c) => String(c[0])).join('\n');
  } finally {
    log.mockRestore();
  }
}

describe('session start', () => {
  test('schedules the upload of a ready session', async () => {
    history = { global: [{ sessionSharing: true, timestamp: 1 }], project: [{ sessionSharing: true, timestamp: 1 }] };
    expect(await run()).toContain('uploading 1 session');
    expect(spawned).toContainEqual(expect.arrayContaining(['upload', 'send', '--sessions', 's1']));
  });

  test('does not count or schedule a session last modified while sharing was off', async () => {
    history = {
      global: [{ sessionSharing: true, timestamp: 1 }],
      project: [
        { sessionSharing: true, timestamp: 1 },
        { sessionSharing: false, timestamp: OLD.getTime() - DAY_MS },
        { sessionSharing: true, timestamp: OLD.getTime() + DAY_MS },
      ],
    };
    expect(await run()).not.toContain('uploading');
    expect(spawned.filter((args) => args[0] === 'upload')).toEqual([]);
  });
});
