import { readFile, rename, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { statePaths } from './config';

const MAX_SNOOZE_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

/** Parse a duration string like "24h", "2d", "30m" into milliseconds. */
export function parseDuration(duration: string): number | null {
  const match = duration.match(/^(\d+)(m|h|d)$/);
  if (!match) return null;
  const value = parseInt(match[1], 10);
  switch (match[2]) {
    case 'm':
      return value * 60 * 1000;
    case 'h':
      return value * 60 * 60 * 1000;
    case 'd':
      return value * 24 * 60 * 60 * 1000;
    default:
      return null;
  }
}

/** Read the snooze-until timestamp from the state dir. Returns null if not snoozed or expired. */
export async function getSnoozeUntil(stateDir: string): Promise<number | null> {
  try {
    const content = await readFile(statePaths(stateDir).snoozeUntil, 'utf-8');
    const timestamp = parseInt(content.trim(), 10);
    if (isNaN(timestamp)) return null;
    if (Date.now() >= timestamp) return null;
    return timestamp;
  } catch {
    return null;
  }
}

/** Set the snooze-until timestamp. Uses atomic write (temp file + rename). */
export async function setSnooze(stateDir: string, durationMs: number): Promise<number> {
  const capped = Math.min(durationMs, MAX_SNOOZE_MS);
  const until = Date.now() + capped;
  const tmpFile = join(tmpdir(), `snooze-until-${Date.now()}`);
  await writeFile(tmpFile, String(until));
  await rename(tmpFile, statePaths(stateDir).snoozeUntil);
  return until;
}

/** Clear the snooze. */
export async function clearSnooze(stateDir: string): Promise<boolean> {
  try {
    await unlink(statePaths(stateDir).snoozeUntil);
    return true;
  } catch {
    return false;
  }
}

/** Check if uploads are currently snoozed. */
export async function isSnoozed(stateDir: string): Promise<boolean> {
  return (await getSnoozeUntil(stateDir)) !== null;
}
