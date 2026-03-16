import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, getConfig, getOrCreateCheckoutId, loadTranscriptsDirs } from '../lib/config';
import { heartbeatSession } from '../lib/convex';
import { countRawLines } from '../lib/extraction';
import { discoverSessions } from '../lib/session-state';

export async function hiveHeartbeat(): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);

  const status = await checkAuthStatus(true);
  if (!status.authenticated) return 1;

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) return 0;

  const [checkoutId, allSessions] = await Promise.all([
    getOrCreateCheckoutId(stateDir),
    discoverSessions(transcriptsDirs),
  ]);
  const project = getCanonicalProjectName(cwd);

  let failures = 0;
  for (const s of allSessions) {
    let messageCount: number;
    try {
      messageCount = await countRawLines(s.path);
    } catch {
      continue;
    }

    try {
      await heartbeatSession({
        sessionId: s.sessionId,
        checkoutId,
        project,
        lineCount: messageCount,
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
