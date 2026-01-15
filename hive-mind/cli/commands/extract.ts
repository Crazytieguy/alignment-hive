import { extractSingleSession } from "../lib/extraction";

export async function extract(): Promise<number> {
  const cwd = process.env.CWD || process.cwd();
  const sessionIds = process.argv.slice(3);

  if (sessionIds.length === 0) {
    return 1;
  }

  let failures = 0;
  for (const sessionId of sessionIds) {
    try {
      const success = await extractSingleSession(cwd, sessionId);
      if (!success) failures++;
    } catch (error) {
      if (process.env.DEBUG) {
        console.error(`[extract] ${error instanceof Error ? error.message : String(error)}`);
      }
      failures++;
    }
  }
  return failures > 0 ? 1 : 0;
}
