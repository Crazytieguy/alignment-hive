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
let isLoginExpired = false;
mockForFile('../lib/auth', realAuth, {
  getAuthData: () =>
    isLoginExpired ? Promise.reject(new Error('refresh failed')) : Promise.resolve({ accessToken: 'test' }),
});
mockForFile('../lib/hook-input', realHookInput, { readHookInput: () => Promise.resolve({ cwd: root }) });
mockForFile('../lib/spawn', realSpawn, {
  spawnBackgroundCommand: (args: Array<string>) => {
    spawned.push(args);
    return true;
  },
});

const { hiveSessionStart } = await import('../commands/hive-session-start');
const { hiveNotices } = await import('../commands/notices');
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

async function printed(command: () => Promise<number>): Promise<string> {
  const log = spyOn(console, 'log').mockImplementation(() => {});
  try {
    await command();
    return log.mock.calls.map((c) => String(c[0])).join('\n');
  } finally {
    log.mockRestore();
  }
}

type Notice = { id: string; text: string; when?: string; actions?: Array<{ id: string }> };

async function notices(sessionId = 's1'): Promise<Array<Notice>> {
  const cwd = process.cwd();
  process.chdir(root);
  try {
    return (JSON.parse(await printed(() => hiveNotices([sessionId]))) as { notices: Array<Notice> }).notices;
  } finally {
    process.chdir(cwd);
  }
}

const SHARING = { global: [{ sessionSharing: true, timestamp: 1 }], project: [{ sessionSharing: true, timestamp: 1 }] };

describe('session start', () => {
  beforeEach(() => {
    isLoginExpired = false;
    delete process.env.HIVE_PLUGIN_VERSION;
  });

  test('schedules the upload of a ready session and prints nothing; notices shows it', async () => {
    history = SHARING;
    expect(await printed(hiveSessionStart)).toBe('');
    expect(spawned).toContainEqual(expect.arrayContaining(['upload', 'send', '--sessions', 's1']));

    // One row per decision: the upload about to start (Snooze) at session start, this session's
    // own state (Keep private) once the person is working.
    const rows = await notices();
    expect(rows.map((n) => [n.id, n.when, n.actions?.map((a) => a.id)])).toEqual([
      ['uploading', 'start', ['snooze']],
      ['session', 'working', ['keep-private']],
    ]);
    expect(rows[0]?.text).toContain('uploading 1 session');
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
    await printed(hiveSessionStart);
    expect(spawned.filter((args) => args[0] === 'upload')).toEqual([]);
    expect(await notices()).toEqual([]);
  });

  test('an expired login gets a row and schedules nothing', async () => {
    history = SHARING;
    isLoginExpired = true;
    await printed(hiveSessionStart);
    expect(spawned.filter((args) => args[0] === 'upload')).toEqual([]);
    expect((await notices()).map((n) => n.id)).toEqual(['login-expired']);
  });

  test('notices says whether it knows the session, so the band asks again only when it does not', async () => {
    history = SHARING;
    const cwd = process.cwd();
    process.chdir(root);
    try {
      const known = JSON.parse(await printed(() => hiveNotices(['s1']))) as { isSessionKnown: boolean };
      const unknown = JSON.parse(await printed(() => hiveNotices(['not-yet-discovered']))) as { isSessionKnown: boolean };
      expect([known.isSessionKnown, unknown.isSessionKnown]).toEqual([true, false]);
    } finally {
      process.chdir(cwd);
    }
  });

  test('--session-only gives this session alone, without the project-wide upload rows', async () => {
    history = SHARING;
    await printed(hiveSessionStart);
    const cwd = process.cwd();
    process.chdir(root);
    try {
      const only = JSON.parse(await printed(() => hiveNotices(['s1', '--session-only']))) as {
        notices: Array<Notice>;
        isSessionKnown: boolean;
      };
      expect(only.notices.map((n) => n.id)).toEqual(['session']);
      expect(only.isSessionKnown).toBe(true);
    } finally {
      process.chdir(cwd);
    }
  });

  test('a plugin never aligned gets the /hive:align row', async () => {
    history = SHARING;
    process.env.HIVE_PLUGIN_VERSION = '0.6.5';
    expect((await notices()).map((n) => n.id)).toContain('align');
  });
});
