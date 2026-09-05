import { rm, writeFile } from 'node:fs/promises';
import { readTimestamp, statePaths } from './config';

const MAX_SNOOZE_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

/** The snooze-until timestamp, or null if not snoozed or expired. */
export async function getSnoozeUntil(stateDir: string): Promise<number | null> {
  const timestamp = await readTimestamp(statePaths(stateDir).snoozeUntil);
  return timestamp !== null && Date.now() < timestamp ? timestamp : null;
}

/** Set the snooze-until timestamp, capped at MAX_SNOOZE_MS. */
export async function setSnooze(stateDir: string, durationMs: number): Promise<number> {
  const until = Date.now() + Math.min(durationMs, MAX_SNOOZE_MS);
  await writeFile(statePaths(stateDir).snoozeUntil, String(until));
  return until;
}

export async function clearSnooze(stateDir: string): Promise<void> {
  await rm(statePaths(stateDir).snoozeUntil, { force: true });
}
