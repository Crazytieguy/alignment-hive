import { writeFile } from 'node:fs/promises';
import { ensureStateDir, getStateDir, statePaths } from '../lib/config';
import { readHookInput } from '../lib/hook-input';
import { isRegistryBackfillDone } from '../lib/registry-backfill';
import { UPLOAD_DELAY_MINUTES, isUploadScheduled, loadSharingSnapshot, summarizeUploads } from '../lib/sharing';
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
  if (eligibleIds.length > 0 && !snapshot.snoozeUntil && !(await isUploadScheduled(stateDir))) {
    const spawned = spawnBackgroundCommand(
      ['upload', 'send', '--delay', String(UPLOAD_DELAY_MINUTES * 60), '--sessions', eligibleIds.join(',')],
      statePaths(stateDir).errorLog,
    );
    // Written only after a successful spawn: the child sleeps --delay first, so this cannot race it.
    if (spawned) await writeFile(statePaths(stateDir).uploadScheduled, String(Date.now()));
  }

  if (snapshot.state.parentSessions.length > 0) {
    spawnBackgroundCommand(['heartbeat'], statePaths(stateDir).errorLog);
  }

  return 0;
}
