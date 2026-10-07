import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { acquirePidLock, releasePidLock, withPidLock } from '../lib/pid-lock';

let dir: string;
let lock: string;
const DEAD_PID = '2147483646'; // no such process

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'hive-pid-lock-'));
  lock = join(dir, 'lock');
});

afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

describe('pid lock', () => {
  test('of many racing acquires exactly one wins, and the lock always holds its pid', async () => {
    // The race this guards against lost about one round in five before the fix.
    for (let round = 0; round < 10; round++) {
      const results = await Promise.all(Array.from({ length: 20 }, () => acquirePidLock(lock)));
      expect(results.filter(Boolean)).toHaveLength(1);
      expect(await readFile(lock, 'utf-8')).toBe(String(process.pid));
      expect(await readdir(dir)).toEqual(['lock']); // no temp files left behind
      await releasePidLock(lock);
    }
  });

  test('of many contenders taking over a dead process lock, exactly one wins', async () => {
    for (let round = 0; round < 10; round++) {
      await writeFile(lock, DEAD_PID);
      const results = await Promise.all(Array.from({ length: 20 }, () => acquirePidLock(lock)));
      expect(results.filter(Boolean)).toHaveLength(1);
      expect(await readFile(lock, 'utf-8')).toBe(String(process.pid));
      expect(await readdir(dir)).toEqual(['lock']); // no guard or temp files left behind
      await releasePidLock(lock);
    }
  });

  test('releasing does not remove a lock another process has since taken', async () => {
    await writeFile(lock, String(process.ppid));
    await releasePidLock(lock);
    expect(await readFile(lock, 'utf-8')).toBe(String(process.ppid));
  });

  test('a recovery guard left by a dead process delays the takeover by one attempt', async () => {
    await writeFile(lock, DEAD_PID);
    await writeFile(`${lock}.recover`, DEAD_PID);
    expect(await acquirePidLock(lock)).toBe(false);
    expect(await acquirePidLock(lock)).toBe(true);
    expect(await readdir(dir)).toEqual(['lock']);
  });

  test('a lock held by a live process is not taken; one left by a dead process is', async () => {
    await writeFile(lock, String(process.pid));
    expect(await acquirePidLock(lock)).toBe(false);

    await writeFile(lock, DEAD_PID);
    expect(await acquirePidLock(lock)).toBe(true);
    expect(await readFile(lock, 'utf-8')).toBe(String(process.pid));
  });

  test('withPidLock releases after the work, and skips it while the lock is held', async () => {
    expect(await withPidLock(lock, () => Promise.resolve('ran'))).toBe('ran');
    expect(await readdir(dir)).toEqual([]);

    expect(await acquirePidLock(lock)).toBe(true);
    expect(await withPidLock(lock, () => Promise.resolve('ran'))).toBeNull();
    await releasePidLock(lock);
  });
});
