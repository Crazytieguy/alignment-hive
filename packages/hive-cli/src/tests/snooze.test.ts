import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, test } from 'bun:test';
import { statePaths } from '../lib/config';
import { clearSnooze, getSnoozeUntil, setSnooze } from '../lib/snooze';

describe('snooze', () => {
  test('an expired or unparseable snooze reads as not snoozed; a future one reads as its timestamp', async () => {
    const stateDir = await mkdtemp(join(tmpdir(), 'hive-snooze-'));
    const file = statePaths(stateDir).snoozeUntil;
    await writeFile(file, String(Date.now() - 1000));
    expect(await getSnoozeUntil(stateDir)).toBeNull();
    await writeFile(file, 'garbage');
    expect(await getSnoozeUntil(stateDir)).toBeNull();
    const future = Date.now() + 60_000;
    await writeFile(file, String(future));
    expect(await getSnoozeUntil(stateDir)).toBe(future);
    await rm(stateDir, { recursive: true, force: true });
  });

  test('setSnooze caps at seven days and clearSnooze removes it', async () => {
    const stateDir = await mkdtemp(join(tmpdir(), 'hive-snooze-'));
    const until = await setSnooze(stateDir, 30 * 24 * 60 * 60 * 1000);
    expect(until - Date.now()).toBeLessThanOrEqual(7 * 24 * 60 * 60 * 1000);
    expect(await getSnoozeUntil(stateDir)).toBe(until);
    await clearSnooze(stateDir);
    expect(await getSnoozeUntil(stateDir)).toBeNull();
    await clearSnooze(stateDir); // idempotent
    await rm(stateDir, { recursive: true, force: true });
  });
});
