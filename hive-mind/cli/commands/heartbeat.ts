import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName } from '../lib/config';
import { heartbeatSession } from '../lib/convex';
import { getHiveMindSessionsDir, isMetaError, readExtractedMeta } from '../lib/extraction';

export async function heartbeat(): Promise<number> {
  const cwd = process.env.CWD || process.cwd();
  const sessionIds = process.argv.slice(3);

  if (sessionIds.length === 0) {
    return 1;
  }

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    return 1;
  }

  const sessionsDir = getHiveMindSessionsDir(cwd);
  const project = getCanonicalProjectName(cwd);
  let failures = 0;

  for (const sessionId of sessionIds) {
    const metaResult = await readExtractedMeta(join(sessionsDir, `${sessionId}.jsonl`));
    if (!metaResult || isMetaError(metaResult)) {
      failures++;
      continue;
    }

    try {
      await heartbeatSession({
        sessionId: metaResult.sessionId,
        checkoutId: metaResult.checkoutId,
        project,
        lineCount: metaResult.messageCount,
        parentSessionId: metaResult.parentSessionId,
      });
    } catch (error) {
      if (process.env.DEBUG) {
        console.error(`[heartbeat] ${error instanceof Error ? error.message : String(error)}`);
      }
      failures++;
    }
  }
  return failures > 0 ? 1 : 0;
}
