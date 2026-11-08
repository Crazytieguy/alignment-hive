import { ensureStateDir, getStateDir, statePaths } from '../lib/config';
import { readHookInput } from '../lib/hook-input';
import { isRegistryBackfillDone } from '../lib/registry-backfill';
import {
  UPLOAD_DELAY_MINUTES,
  loadSharingSnapshot,
  pickUploadBatch,
  scheduleUpload,
  summarizeUploads,
} from '../lib/sharing';
import { spawnBackgroundCommand } from '../lib/spawn';

/**
 * The SessionStart hook's work: background pings, scans and uploads. It prints nothing; the hive
 * plugin's band shows what the person should know, through `hive notices`.
 */
export async function hiveSessionStart(): Promise<number> {
  const hookInput = await readHookInput();
  const cwd = hookInput.cwd || process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  // Detached so startup never waits on the network; an in-process request would either block
  // the hook or be cut off by process.exit. Before the sharing opt-out on purpose: the ping
  // counts installs, sharing or not.
  spawnBackgroundCommand(['checkout-ping'], statePaths(stateDir).errorLog);

  // One-time scan for transcript dirs the hooks never registered (see registry-backfill.ts).
  // Detached: it reads a line from every dir under ~/.claude/projects. Before the sharing and
  // login gates on purpose: `hive local` retrieval reads the registry without either.
  if (!isRegistryBackfillDone(stateDir)) {
    spawnBackgroundCommand(['registry-backfill'], statePaths(stateDir).errorLog);
  }

  const snapshot = await loadSharingSnapshot(stateDir, cwd);
  if (snapshot.kind !== 'on') return 0;

  const { eligibleIds } = summarizeUploads(snapshot);
  if (eligibleIds.length > 0 && !snapshot.snoozeUntil) {
    // A capped batch: the ids travel as one argument. The job clears the marker when it ends, so
    // the next session start after that schedules the next batch.
    const batch = pickUploadBatch(eligibleIds).join(',');
    const delay = String(UPLOAD_DELAY_MINUTES * 60);
    await scheduleUpload(stateDir, (scheduledAt) => {
      const job = ['upload', 'send', '--delay', delay, '--sessions', batch, '--scheduled-at', String(scheduledAt)];
      return spawnBackgroundCommand(job, statePaths(stateDir).errorLog) !== null;
    });
  }

  if (snapshot.state.parentSessions.length > 0) {
    spawnBackgroundCommand(['heartbeat'], statePaths(stateDir).errorLog);
  }

  return 0;
}
