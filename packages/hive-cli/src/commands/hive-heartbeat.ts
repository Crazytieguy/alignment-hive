import { getAuthData } from '../lib/auth';
import {
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  getStateDir,
  isSharingDisabledLocally,
  loadTranscriptsDirs,
  statePaths,
} from '../lib/config';
import { heartbeatSession } from '../lib/convex';
import { withPidLock } from '../lib/pid-lock';
import { countRawLines } from '../lib/session-io';
import { computeSessionStatus, loadSessionState, withDiscoveryCache } from '../lib/session-state';

export async function hiveHeartbeat(): Promise<number> {
  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);

  if (isSharingDisabledLocally(stateDir)) return 0;
  // Every session start spawns a heartbeat. When several start together, only one scans; the
  // others exit, and whatever changes after its scan goes with the next session start's.
  return (await withPidLock(statePaths(stateDir).heartbeatLock, () => sendHeartbeats(cwd, stateDir))) ?? 0;
}

async function sendHeartbeats(cwd: string, stateDir: string): Promise<number> {
  const authData = await getAuthData();
  if (!authData) return 1;

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) return 0;

  const [checkoutId, state] = await Promise.all([
    getOrCreateCheckoutId(stateDir),
    withDiscoveryCache(stateDir, (cache) => loadSessionState(stateDir, transcriptsDirs, cwd, cache)),
  ]);
  const ids = getProjectIdentifiers(cwd);
  // Only the uploaded/excluded verdicts are read, so the review-period inputs are left neutral.
  // An upload's own save already records its line count, and an excluded session sends nothing;
  // a session resumed after its upload is no longer 'uploaded' and gets heartbeats again.
  const statusCtx = { ...state, consentMtime: 0, snoozeUntil: null };
  const unsettled = state.parentSessions.filter((s) => {
    const { type } = computeSessionStatus(s, statusCtx);
    return type !== 'uploaded' && type !== 'excluded';
  });

  let failures = 0;
  for (const s of unsettled) {
    let lineCount: number;
    try {
      lineCount = await countRawLines(s.path);
    } catch {
      continue; // file vanished between discovery and read
    }

    // A `hive consent disable` during a long scan stops the remaining heartbeats.
    if (isSharingDisabledLocally(stateDir)) return 0;
    try {
      await heartbeatSession({
        sessionId: s.sessionId,
        checkoutId,
        directory: ids.directory,
        gitRemote: ids.gitRemote,
        lineCount,
        lastModified: s.mtime.getTime(),
      });
    } catch (error) {
      if (process.env.DEBUG) {
        console.error(`[hive-heartbeat] ${error instanceof Error ? error.message : String(error)}`);
      }
      failures++;
    }
  }

  return failures > 0 ? 1 : 0;
}
