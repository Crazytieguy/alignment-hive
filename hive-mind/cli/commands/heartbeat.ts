import { join } from "node:path";
import { getCanonicalProjectName } from "../lib/config";
import { heartbeatSession } from "../lib/convex";
import { getHiveMindSessionsDir, readExtractedMeta } from "../lib/extraction";

export async function heartbeat(): Promise<number> {
  const cwd = process.env.CWD || process.cwd();
  const sessionId = process.argv[3];

  if (!sessionId) {
    return 1;
  }

  const sessionsDir = getHiveMindSessionsDir(cwd);
  const meta = await readExtractedMeta(join(sessionsDir, `${sessionId}.jsonl`));

  if (!meta) {
    return 1;
  }

  const project = getCanonicalProjectName(cwd);

  try {
    await heartbeatSession({
      sessionId: meta.sessionId,
      checkoutId: meta.checkoutId,
      project,
      lineCount: meta.messageCount,
      parentSessionId: meta.parentSessionId,
    });
    return 0;
  } catch (error) {
    if (process.env.DEBUG) {
      console.error(`[heartbeat] ${error instanceof Error ? error.message : String(error)}`);
    }
    return 1;
  }
}
