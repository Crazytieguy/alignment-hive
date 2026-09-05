import { getAuthData } from '../lib/auth';
import {
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  getStateDir,
  isSharingDisabledLocally,
  loadTranscriptsDirs,
} from '../lib/config';
import { heartbeatSession } from '../lib/convex';
import { countRawLines } from '../lib/session-io';
import { discoverSessions } from '../lib/session-state';

export async function hiveHeartbeat(): Promise<number> {
  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);

  if (isSharingDisabledLocally(stateDir)) return 0;
  const authData = await getAuthData();
  if (!authData) return 1;

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) return 0;

  const [checkoutId, allSessions] = await Promise.all([
    getOrCreateCheckoutId(stateDir),
    discoverSessions(transcriptsDirs, cwd),
  ]);
  const ids = getProjectIdentifiers(cwd);

  let failures = 0;
  for (const s of allSessions.filter((session) => !session.agentId)) {
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
