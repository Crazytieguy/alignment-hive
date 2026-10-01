import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import * as realAuth from '../lib/auth';
import * as realConvex from '../lib/convex';
import { mockForFile } from './file-mock';

// Every network entry point is stubbed; resolveProjectConsent is the first step of any upload run,
// so a call to it means the command reached the upload path.
const calls: Array<string> = [];
const blocked = (name: string) => () => {
  calls.push(name);
  throw new Error(`${name} must not be called`);
};
mockForFile('../lib/convex', realConvex, {
  resolveProjectConsent: (cwd: string) => {
    calls.push('resolveProjectConsent');
    return Promise.resolve({ consentMtime: 0, ids: { directory: cwd } });
  },
  getConsentHistory: () => {
    calls.push('getConsentHistory');
    return Promise.resolve({ global: [], project: [] });
  },
  generateUploadUrls: blocked('generateUploadUrls'),
  saveUploads: blocked('saveUploads'),
  saveWorkflowRuns: blocked('saveWorkflowRuns'),
});
mockForFile('../lib/auth', realAuth, { getAuthData: blocked('getAuthData') });

const { uploadSend } = await import('../commands/upload-send');
const { uploadExclude } = await import('../commands/upload-exclude');
const { uploadSnooze } = await import('../commands/upload-snooze');
const { uploadList } = await import('../commands/upload-list');
const { statePaths } = await import('../lib/config');

let root = '';
let stateDir = '';
const originalCwd = process.cwd();

beforeAll(async () => {
  root = await realpath(await mkdtemp(join(tmpdir(), 'hive-upload-cmd-')));
  stateDir = join(root, '.claude', 'hive');
  process.chdir(root);
});

afterAll(async () => {
  process.chdir(originalCwd);
  await rm(root, { recursive: true, force: true });
});

beforeEach(async () => {
  calls.length = 0;
  await rm(join(root, '.claude'), { recursive: true, force: true });
});

const quiet = async (run: () => Promise<number>) => {
  const log = spyOn(console, 'log').mockImplementation(() => {});
  const error = spyOn(console, 'error').mockImplementation(() => {});
  try {
    return await run();
  } finally {
    log.mockRestore();
    error.mockRestore();
  }
};

describe('upload send argument parsing', () => {
  // Each of these once fell through to a manual batch that uploads every pending session.
  const usageErrors = [
    ['--dry-run'],
    ['--sesions', 'abc'],
    ['--sessions'],
    ['--delay'],
    ['--delay', '--sessions', 'abc'],
    ['--delay='],
    ['--sessions=', 'abc'],
    ['--delay', 'abc'],
    ['--delay', '5', '-x'],
    ['abc', 'def'],
    [''],
    ['abc', '--sessions', 'def'],
    ['abc', '--delay', '5'],
  ];
  for (const args of usageErrors) {
    test(`send ${args.join(' ')} is a usage error that touches nothing`, async () => {
      expect(await quiet(() => uploadSend(args))).toBe(2);
      expect(calls).toEqual([]);
      expect(existsSync(stateDir)).toBe(false);
    });
  }

  for (const flag of ['--help', '-h']) {
    test(`send ${flag} prints usage and touches nothing`, async () => {
      expect(await quiet(() => uploadSend([flag, 'abc']))).toBe(0);
      expect(calls).toEqual([]);
      expect(existsSync(stateDir)).toBe(false);
    });
  }

  test('--delay 0 is the background job: it honours a snooze, which a manual send ignores', async () => {
    await mkdir(stateDir, { recursive: true });
    await writeFile(statePaths(stateDir).snoozeUntil, String(Date.now() + 60_000));
    expect(await quiet(() => uploadSend(['--delay', '0', '--sessions', 'abc']))).toBe(0);
    expect(calls).toEqual([]);

    // Control: the stub does see a run that reaches the upload path.
    expect(await quiet(() => uploadSend(['--sessions', 'abc']))).toBe(0);
    expect(calls).toContain('resolveProjectConsent');
  });
});

describe('other upload commands parse strictly', () => {
  const cases: Array<[string, (args: Array<string>) => Promise<number>, Array<string>, number]> = [
    ['exclude', uploadExclude, [], 2],
    ['exclude', uploadExclude, ['abc', '--all'], 2],
    ['exclude', uploadExclude, ['abc', 'def'], 2],
    ['exclude', uploadExclude, ['--al'], 2],
    ['exclude', uploadExclude, ['-h'], 0],
    ['snooze', uploadSnooze, ['1h', '--clear'], 2],
    ['snooze', uploadSnooze, ['1h', '2h'], 2],
    ['snooze', uploadSnooze, ['--clean'], 2],
    ['snooze', uploadSnooze, ['soon'], 1],
    ['snooze', uploadSnooze, ['--help'], 0],
    ['list', uploadList, ['abc'], 2],
    ['list', uploadList, ['--everything'], 2],
    ['list', uploadList, ['--help'], 0],
  ];
  for (const [name, run, args, code] of cases) {
    test(`${name} ${args.join(' ') || '(no arguments)'} exits ${code} before touching state`, async () => {
      expect(await quiet(() => run(args))).toBe(code);
      expect(calls).toEqual([]);
      expect(existsSync(stateDir)).toBe(false);
    });
  }
});
